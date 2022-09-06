#![feature(generic_associated_types, never_type, type_alias_impl_trait)]

use std::fmt;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

mod monroe {
    pub mod ring;
}

fn get_specs() -> Vec<RingSpec> {
    [(10, 100), (100, 1_000)]
        .into_iter()
        .map(|(nodes, rounds)| RingSpec::new(nodes, rounds))
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub struct RingSpec {
    /// The number of actors in the ring.
    pub nodes: i32,
    /// Amount of times a payload will be sent
    /// around the ring.
    pub rounds: i32,
    /// Total number of messages passed around.
    pub limit: i32,
}

impl RingSpec {
    fn new(nodes: i32, rounds: i32) -> Self {
        Self {
            nodes,
            rounds,
            limit: nodes * rounds,
        }
    }
}

impl fmt::Display for RingSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} rounds through {} nodes", self.rounds, self.nodes)
    }
}

fn bench_monroe(c: &mut Criterion) {
    let mut group = c.benchmark_group("monroe");

    for spec in get_specs() {
        let id = BenchmarkId::from_parameter(spec);
        group.bench_with_input(id, &spec, |b, spec| b.iter(|| monroe::ring::run(*spec)));
    }
}

criterion_group!(ring, bench_monroe);
criterion_main!(ring);
