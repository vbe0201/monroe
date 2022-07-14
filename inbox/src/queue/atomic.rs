use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Semaphore(AtomicUsize);

impl Semaphore {
    pub const fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    pub fn may_claim(&self, limit: usize) -> bool {
        self.0.load(Ordering::Relaxed) < limit
    }

    pub fn try_claim(&self, limit: usize) -> bool {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            let mut backoff = crate::spin::Backoff::default();
            loop {
                if self.0.fetch_add(1, Ordering::Acquire) < limit {
                    return true;
                }

                backoff.spin();

                if self.0.fetch_sub(1, Ordering::Relaxed) > limit {
                    return false;
                }

                backoff.spin();
            }
        }

        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        return fetch_update(&self.0, Ordering::Acquire, |v| (v < limit).then(|| v + 1)).is_ok();
    }

    pub fn release(&self) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        self.0.fetch_sub(1, Ordering::Release);

        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        fetch_update(&self.0, Ordering::Release, |v| Some(v - 1)).unwrap();
    }
}

pub struct Counter(AtomicUsize);

impl Counter {
    pub const fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    pub fn fetch_inc(&self) -> usize {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        return self.0.fetch_add(1, Ordering::Relaxed);

        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        return fetch_update(&self.0, Ordering::Relaxed, |v| Some(v + 1)).unwrap();
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
#[inline(always)]
fn fetch_update(
    value: &AtomicUsize,
    success: Ordering,
    mut update: impl FnMut(usize) -> Option<usize>,
) -> Result<usize, usize> {
    loop {
        let v = value.load(Ordering::Relaxed);
        let new_v = update(v).ok_or(v)?;
        match value.compare_exchange(v, new_v, success, Ordering::Relaxed) {
            Ok(_) => return Ok(v),
            Err(_) => std::thread::yield_now(),
        }
    }
}
