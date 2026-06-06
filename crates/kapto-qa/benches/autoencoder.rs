// SPDX-License-Identifier: Apache-2.0

//! How long learning and using the linear autoencoder takes, across how
//! many episodes it learns from and how many statistics each has: an arm
//! with a gripper has about 30, a pair of arms with force sensors a few
//! hundred. The linear one does one decomposition per episode, to learn what
//! error is usual, so it grows faster than the episodes do; the neural one
//! learns six times (five folds and the model), each by gradient descent. Run with
//! `cargo bench -p kapto-qa --bench autoencoder`; results land in
//! `target/criterion/`.

#![allow(
    clippy::unwrap_used,
    reason = "benchmark setup fails loudly when the setup itself breaks"
)]

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::time::{Duration, Instant};

use kapto_qa::autoencoder::LinearAutoencoder;
use kapto_qa::autoencoder::neural::NeuralAutoencoder;
use kapto_qa::episode_statistics::{Statistic, Statistics};

/// Episodes learned from: the fewest worth learning from up to the most a
/// model keeps.
const EPISODES: [usize; 4] = [20, 50, 100, 200];
/// Statistics per episode.
const STATISTICS: [usize; 3] = [30, 100, 300];

/// `count` episodes of `statistics` single values each, which vary together
/// in a few ways and a little on their own, as a task's episodes do.
fn episodes(count: usize, statistics: usize) -> Vec<Statistics> {
    (0..count)
        .map(|episode| {
            #[allow(clippy::cast_precision_loss, reason = "small counts")]
            let (e, ways) = (episode as f64, [0.3, 1.1, 2.3]);
            (0..statistics)
                .map(|index| {
                    #[allow(clippy::cast_precision_loss, reason = "small counts")]
                    let i = index as f64;
                    let together: f64 = ways.iter().map(|way| (e * way + i).sin()).sum();
                    let alone = 0.05 * (e * 7.7 + i * 3.1).cos();
                    (
                        format!("statistic.{index:03}"),
                        Statistic::Value(10.0 + together + alone),
                    )
                })
                .collect()
        })
        .collect()
}

fn learning(c: &mut Criterion) {
    let mut group = c.benchmark_group("autoencoder");
    group.sample_size(10);
    for statistics in STATISTICS {
        for count in EPISODES {
            let learned_from = episodes(count, statistics);
            group.bench_with_input(
                BenchmarkId::new(format!("train/{statistics}-statistics"), count),
                &learned_from,
                |b, learned_from| b.iter(|| LinearAutoencoder::train(black_box(learned_from), 0.9)),
            );
            group.bench_with_input(
                BenchmarkId::new(format!("train_neural/{statistics}-statistics"), count),
                &learned_from,
                |b, learned_from| {
                    b.iter(|| {
                        let limit = Instant::now() + Duration::from_secs(600);
                        NeuralAutoencoder::train(black_box(learned_from), 16, limit)
                    });
                },
            );
            let model = LinearAutoencoder::train(&learned_from, 0.9).unwrap();
            group.bench_with_input(
                BenchmarkId::new(format!("score/{statistics}-statistics"), count),
                &learned_from[0],
                |b, episode| b.iter(|| model.reconstruct(black_box(episode))),
            );
        }
    }
    group.finish();
}

criterion_group!(benches, learning);
criterion_main!(benches);
