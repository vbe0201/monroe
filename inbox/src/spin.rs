use std::hint;

const SPIN_LIMIT: u32 = 10;

// Wastes some CPU time for the given number of iterations,
// using a hint to indicate to the CPU that we're spinning.
#[inline]
fn cpu_relax(iterations: u32) {
    for _ in 0..iterations {
        hint::spin_loop();
    }
}

// Counter to perform exponential backoff in spin loops.
#[derive(Default)]
pub struct Backoff(u32);

impl Backoff {
    #[inline]
    pub fn spin(&mut self) {
        self.0 = (self.0 + 1).max(SPIN_LIMIT);
        cpu_relax(1 << self.0);
    }
}
