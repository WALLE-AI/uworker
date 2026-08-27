//! 宿主义务 H4 与内容降级路径的契约测试（架构 §4.2、§12.2）。
//!
//! > `ContentStore` 不得在 `retain` 有效期内回收内容。
//! > 违反后果：模型请求不可重建。
//!
//! 另外覆盖三条降级路径——它们是**可预期路径，不是 panic**：
//! `NotFound` 降级为占位、`Forbidden` 直接剔除且不暴露存在性、后端错误可重试。

use agentrs_contracts::content::{ContentError, ContentMeta, ContentScope, RetentionOwner};
use agentrs_contracts::ids::EventSequence;
use agentrs_contracts::ports::ContentStore;
use agentrs_testkit::{ContentFault, FakeContentStore};
use bytes::Bytes;

fn 全局() -> ContentScope {
    ContentScope::Global
}

#[tokio::test]
async fn h4_1_retain_期间内容不被回收() {
    let s = FakeContentStore::new();
    let r = s
        .put(全局(), Bytes::from("hello"), ContentMeta::default())
        .await
        .unwrap();

    s.retain(
        RetentionOwner::Checkpoint {
            run_id: "r1".into(),
            up_to_seq: EventSequence(5),
        },
        std::slice::from_ref(&r),
    )
    .await
    .unwrap();

    assert_eq!(s.collect_unretained(), 0, "被 retain 的内容不得被回收");
    assert_eq!(s.get(&r).await.unwrap(), Bytes::from("hello"));
}

#[tokio::test]
async fn h4_2_未_retain_的内容会被回收() {
    // 这是"悬空引用"的可观测形式：内核若忘记 retain，GC 后解引用即失败。
    let s = FakeContentStore::new();
    let r = s
        .put(全局(), Bytes::from("orphan"), ContentMeta::default())
        .await
        .unwrap();

    assert_eq!(s.collect_unretained(), 1);
    assert!(s.was_collected(&r.digest));
    assert!(matches!(s.get(&r).await, Err(ContentError::NotFound { .. })));
}

#[tokio::test]
async fn h4_3_release_之后才可回收() {
    let s = FakeContentStore::new();
    let r = s
        .put(全局(), Bytes::from("x"), ContentMeta::default())
        .await
        .unwrap();
    let owner = RetentionOwner::Run { run_id: "r1".into() };

    s.retain(owner.clone(), std::slice::from_ref(&r)).await.unwrap();
    assert_eq!(s.collect_unretained(), 0);

    s.release(owner).await.unwrap();
    assert_eq!(s.collect_unretained(), 1, "释放后方可回收");
}

#[tokio::test]
async fn h4_4_多_owner_retain_同一内容时任一存活即保留() {
    let s = FakeContentStore::new();
    let r = s
        .put(全局(), Bytes::from("shared"), ContentMeta::default())
        .await
        .unwrap();

    let a = RetentionOwner::Run { run_id: "r1".into() };
    let b = RetentionOwner::Checkpoint {
        run_id: "r2".into(),
        up_to_seq: EventSequence(1),
    };
    s.retain(a.clone(), std::slice::from_ref(&r)).await.unwrap();
    s.retain(b, std::slice::from_ref(&r)).await.unwrap();

    s.release(a).await.unwrap();
    assert_eq!(s.collect_unretained(), 0, "还有一个 owner 持有，不得回收");
}

#[tokio::test]
async fn 相同字节产生相同引用() {
    // 内容寻址的基本性质：去重与 replay 一致性都依赖它。
    let s = FakeContentStore::new();
    let a = s
        .put(全局(), Bytes::from("same"), ContentMeta::default())
        .await
        .unwrap();
    let b = s
        .put(全局(), Bytes::from("same"), ContentMeta::default())
        .await
        .unwrap();
    assert_eq!(a, b);
    assert_eq!(s.blob_count(), 1, "相同内容不重复存储");
}

#[tokio::test]
async fn not_found_是可预期的降级路径() {
    let s = FakeContentStore::new();
    let r = s
        .put(全局(), Bytes::from("gone"), ContentMeta::default())
        .await
        .unwrap();
    s.inject(ContentFault::NotFound(r.digest.clone()));

    match s.get(&r).await {
        Err(ContentError::NotFound { digest }) => {
            assert_eq!(digest, r.digest);
            // 内核据此降级为占位摘要，保留原长度与来源——len 仍在 ref 上可读。
            assert_eq!(r.len, 4);
        }
        other => panic!("期望 NotFound，得到 {other:?}"),
    }
}

#[tokio::test]
async fn forbidden_不泄漏任何内容信息() {
    let s = FakeContentStore::new();
    let r = s
        .put(全局(), Bytes::from("secret"), ContentMeta::default())
        .await
        .unwrap();
    s.inject(ContentFault::Forbidden(r.digest.clone()));

    let err = s.get(&r).await.unwrap_err();
    assert!(matches!(err, ContentError::Forbidden));
    // Forbidden 变体没有字段——从类型上就无法泄漏长度等旁路信息。
    // 内核对它的处理是"直接剔除"，不向模型暴露该 fragment 曾存在。
    assert_eq!(format!("{err}"), "content forbidden");
}

#[tokio::test]
async fn 后端错误按可重试处理() {
    let s = FakeContentStore::new();
    let r = s
        .put(全局(), Bytes::from("x"), ContentMeta::default())
        .await
        .unwrap();
    s.inject(ContentFault::Backend);

    assert!(matches!(s.get(&r).await, Err(ContentError::Backend { .. })));
    // 故障是一次性的：重试即成功。
    assert_eq!(s.get(&r).await.unwrap(), Bytes::from("x"));
}

#[tokio::test]
async fn 范围读取可用于大内容分片() {
    let s = FakeContentStore::new();
    let r = s
        .put(全局(), Bytes::from("0123456789"), ContentMeta::default())
        .await
        .unwrap();
    let part = s
        .get_range(&r, agentrs_contracts::content::ByteRange { start: 2, end: 5 })
        .await
        .unwrap();
    assert_eq!(part, Bytes::from("234"));
}
