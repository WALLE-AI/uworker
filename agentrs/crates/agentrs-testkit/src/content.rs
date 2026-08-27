//! 内存 fake ContentStore（任务 T02A）。
//!
//! 同时是**宿主义务 H4 的被测对象**：`retain` 有效期内内容不得被回收。
//!
//! 可注入三类故障，用于验证内核的降级路径（架构 §4.2）：
//!
//! | 故障 | 内核应有的反应 |
//! |---|---|
//! | `NotFound` | 降级为占位摘要，保留原长度与来源 |
//! | `Forbidden` | **直接剔除，不降级、不暴露存在性** |
//! | 后端错误 | 按可重试处理 |

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use agentrs_contracts::content::{
    ByteRange, ContentError, ContentMeta, ContentRef, ContentScope, ContentStat, RetentionOwner,
};
use agentrs_contracts::ids::{Digest, Timestamp};
use agentrs_contracts::ports::ContentStore;
use async_trait::async_trait;
use bytes::Bytes;

/// 可注入的内容故障。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentFault {
    /// 该 digest 的内容表现为已被回收。
    NotFound(Digest),
    /// 该 digest 的内容表现为无权访问。
    Forbidden(Digest),
    /// 下一次操作返回后端错误。
    Backend,
}

#[derive(Default)]
struct State {
    blobs: HashMap<Digest, (Bytes, ContentStat)>,
    retained: HashMap<String, HashSet<Digest>>,
    faults: Vec<ContentFault>,
    /// 已被"GC"的内容，用于验证悬空引用检测。
    collected: HashSet<Digest>,
}

/// 内存内容存储 fake。
#[derive(Default)]
pub struct FakeContentStore {
    state: Mutex<State>,
}

/// 用 blake3 之外的确定性摘要——testkit 不引入额外依赖，
/// 只需保证"相同字节 → 相同 digest"，不需要密码学强度。
fn digest_of(bytes: &[u8]) -> Digest {
    // FNV-1a 64 位，够用且确定。
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    Digest::from_hex(format!("{h:016x}"))
}

fn owner_key(owner: &RetentionOwner) -> String {
    match owner {
        RetentionOwner::Checkpoint { run_id, up_to_seq } => {
            format!("ckpt:{run_id}:{up_to_seq}")
        }
        RetentionOwner::Run { run_id } => format!("run:{run_id}"),
    }
}

impl FakeContentStore {
    /// 新建空存储。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入一次故障。
    pub fn inject(&self, fault: ContentFault) {
        self.state.lock().unwrap().faults.push(fault);
    }

    /// 已存储的对象数。
    pub fn blob_count(&self) -> usize {
        self.state.lock().unwrap().blobs.len()
    }

    /// 模拟一次 GC：回收**未被任何 owner retain** 的内容。
    ///
    /// 返回被回收的数量。若内核产生了未 retain 的悬空引用，
    /// 这个方法会把它变成一次可观测的 `NotFound`。
    pub fn collect_unretained(&self) -> usize {
        let mut s = self.state.lock().unwrap();
        let live: HashSet<Digest> = s.retained.values().flatten().cloned().collect();
        let dead: Vec<Digest> = s.blobs.keys().filter(|d| !live.contains(*d)).cloned().collect();
        for d in &dead {
            s.blobs.remove(d);
            s.collected.insert(d.clone());
        }
        dead.len()
    }

    /// 某个 digest 是否已被回收。
    pub fn was_collected(&self, d: &Digest) -> bool {
        self.state.lock().unwrap().collected.contains(d)
    }

    fn check_fault(state: &mut State, d: &Digest) -> Result<(), ContentError> {
        if let Some(pos) = state.faults.iter().position(|f| match f {
            ContentFault::NotFound(x) | ContentFault::Forbidden(x) => x == d,
            ContentFault::Backend => true,
        }) {
            let f = state.faults.remove(pos);
            return Err(match f {
                ContentFault::NotFound(digest) => ContentError::NotFound { digest },
                ContentFault::Forbidden(_) => ContentError::Forbidden,
                ContentFault::Backend => ContentError::Backend {
                    message: "injected".into(),
                },
            });
        }
        Ok(())
    }
}

#[async_trait]
impl ContentStore for FakeContentStore {
    async fn put(
        &self,
        scope: ContentScope,
        bytes: Bytes,
        meta: ContentMeta,
    ) -> Result<ContentRef, ContentError> {
        let digest = digest_of(&bytes);
        let media_type = meta
            .media_type
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let stat = ContentStat {
            len: bytes.len() as u64,
            media_type: media_type.clone(),
            created_at: Timestamp(0),
        };
        let mut s = self.state.lock().unwrap();
        s.blobs.insert(digest.clone(), (bytes.clone(), stat));
        Ok(ContentRef {
            digest,
            len: bytes.len() as u64,
            media_type,
            scope,
        })
    }

    async fn get(&self, r: &ContentRef) -> Result<Bytes, ContentError> {
        let mut s = self.state.lock().unwrap();
        Self::check_fault(&mut s, &r.digest)?;
        s.blobs
            .get(&r.digest)
            .map(|(b, _)| b.clone())
            .ok_or_else(|| ContentError::NotFound {
                digest: r.digest.clone(),
            })
    }

    async fn get_range(&self, r: &ContentRef, range: ByteRange) -> Result<Bytes, ContentError> {
        let all = self.get(r).await?;
        let start = (range.start as usize).min(all.len());
        let end = (range.end as usize).min(all.len());
        Ok(all.slice(start..end))
    }

    async fn stat(&self, r: &ContentRef) -> Result<ContentStat, ContentError> {
        let mut s = self.state.lock().unwrap();
        Self::check_fault(&mut s, &r.digest)?;
        s.blobs
            .get(&r.digest)
            .map(|(_, st)| st.clone())
            .ok_or_else(|| ContentError::NotFound {
                digest: r.digest.clone(),
            })
    }

    async fn retain(&self, owner: RetentionOwner, refs: &[ContentRef]) -> Result<(), ContentError> {
        let mut s = self.state.lock().unwrap();
        let key = owner_key(&owner);
        let entry = s.retained.entry(key).or_default();
        for r in refs {
            entry.insert(r.digest.clone());
        }
        Ok(())
    }

    async fn release(&self, owner: RetentionOwner) -> Result<(), ContentError> {
        self.state.lock().unwrap().retained.remove(&owner_key(&owner));
        Ok(())
    }
}
