//! 음성 콜백 밖의 독립 작업을 입력 순서대로 병렬 처리합니다.
use std::sync::OnceLock;

use rayon::{ThreadPool, ThreadPoolBuilder, prelude::*};

const MIN_WORK: usize = 16_384;

/// 사용 가능한 코어 수에 맞춘 공용 작업 풀을 준비합니다.
fn pool() -> Option<&'static ThreadPool> {
    static POOL: OnceLock<Option<ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let threads = std::thread::available_parallelism().ok()?.get().min(8);
        if threads < 2 {
            return None;
        }
        ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|index| format!("mmlfold-batch-{index}"))
            .build()
            .ok()
    })
    .as_ref()
}

/// 작업량에 따라 순차 또는 병렬로 계산하고 입력 순서로 결과를 반환합니다.
pub(crate) fn map<T, U, F>(items: &[T], work: usize, operation: F) -> Vec<U>
where
    T: Sync,
    U: Send,
    F: Fn(&T) -> U + Send + Sync,
{
    if work < MIN_WORK || items.len() < 2 {
        return items.iter().map(operation).collect();
    }
    let execute = || {
        items
            .par_iter()
            .with_min_len((items.len() / 32).max(1))
            .map(&operation)
            .collect()
    };
    // 중첩 작업 풀을 만들지 않고 호출자가 제공한 Rayon 풀을 우선합니다.
    if rayon::current_thread_index().is_some() {
        execute()
    } else if let Some(pool) = pool() {
        pool.install(execute)
    } else {
        items.iter().map(operation).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// 작업 순서와 첫 오류가 병렬 실행 순서에 영향을 받지 않는지 검사합니다.
    fn scheduling_cannot_change_item_or_first_error_order() {
        let items: Vec<_> = (0..20_000).collect();
        for threads in [1, 4] {
            let pool = ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let result = pool.install(|| {
                map(&items, items.len(), |&index| {
                    if index == 17 || index == 18_000 {
                        Err(index)
                    } else {
                        Ok(index * 2)
                    }
                })
            });
            assert_eq!(result[100], Ok(200));
            assert_eq!(result.into_iter().collect::<Result<Vec<_>, _>>(), Err(17));
        }
    }
}
