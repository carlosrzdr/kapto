// SPDX-License-Identifier: Apache-2.0

//! A neural autoencoder of episodes' statistics: it learns curved relations
//! the linear one can't, such as effort growing with the square of speed.
//!
//! ```text
//! statistics → neurons (tanh) → code → neurons (tanh) → statistics
//! ```
//!
//! The code is a quarter as many numbers as there are statistics, from 1 to
//! 8. It learns by gradient descent (Adam) over all its episodes at once,
//! from weights drawn with a fixed seed, so the same episodes give the same
//! model; it stops once the error stops falling, after at most
//! [`MAX_EPOCHS`] passes, or at a time limit. What error is usual comes from
//! five models each learned without a fifth of the episodes.

use std::time::Instant;

use nalgebra::{DMatrix, DVector};
use serde::{Deserialize, Serialize};

use super::{
    Reconstruction, compare, error_and_values, standardisation, standardised, table, usual,
};
use crate::episode_statistics::Statistics;

/// The most passes over the episodes one learning makes.
pub const MAX_EPOCHS: usize = 10_000;

/// Passes without the error falling by a ten-thousandth after which
/// learning stops.
const PATIENCE: usize = 300;

/// How many groups the episodes are split into to learn what error is
/// usual: each is reconstructed by a model learned from the others.
const FOLDS: usize = 5;

/// The seed the first weights are drawn with.
const SEED: u64 = 0x6b61_7074_6f00_0001;

/// Adam's step size and the decay of its two running averages.
const STEP: f64 = 0.01;
const DECAY: (f64, f64) = (0.9, 0.999);

/// Learning stopped at its time limit before it had finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeLimit;

/// One layer's weights, one row per output, and its biases.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    /// Its inputs.
    pub inputs: usize,
    /// Its outputs.
    pub outputs: usize,
    /// The weights, by output then input.
    pub weights: Vec<f64>,
    /// One bias per output.
    pub biases: Vec<f64>,
}

/// A neural autoencoder of episodes' statistics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NeuralAutoencoder {
    /// The statistics it reconstructs, in order: those every episode it
    /// learned from had.
    pub names: Vec<String>,
    /// Each statistic's mean over the episodes it learned from.
    pub means: Vec<f64>,
    /// Each statistic's scale, as for the linear autoencoder.
    pub scales: Vec<f64>,
    /// Its four layers, input to output.
    pub layers: Vec<Layer>,
    /// How many episodes it learned from.
    pub episodes: usize,
    /// How many passes over them learning took.
    pub epochs: usize,
    /// The median of the errors of reconstructing each episode with a model
    /// that didn't learn from it.
    pub error_median: f64,
    /// Their spread, as for the linear autoencoder.
    pub error_spread: f64,
}

/// The network as it learns: weights and biases per layer.
type Network = Vec<(DMatrix<f64>, DVector<f64>)>;

impl NeuralAutoencoder {
    /// Learns from `episodes` with `neurons` in each hidden layer, unless
    /// `deadline` passes first. `Ok(None)` with fewer episodes than
    /// `FOLDS` (5) or no statistic they all have.
    ///
    /// # Errors
    ///
    /// [`TimeLimit`] if `deadline` passed before it had learned.
    pub fn train(
        episodes: &[Statistics],
        neurons: usize,
        deadline: Instant,
    ) -> Result<Option<Self>, TimeLimit> {
        if episodes.len() < FOLDS {
            return Ok(None);
        }
        let Some((names, values)) = table(episodes) else {
            return Ok(None);
        };
        let mut errors = Vec::with_capacity(values.len());
        for fold in 0..FOLDS {
            let (held, learned): (Vec<_>, Vec<_>) = values
                .iter()
                .enumerate()
                .partition(|(index, _)| index % FOLDS == fold);
            let learned: Vec<Vec<f64>> = learned.into_iter().map(|(_, row)| row.clone()).collect();
            let without = fit(names.clone(), &learned, neurons, deadline)?;
            errors.extend(held.into_iter().map(|(_, row)| without.error_of(row).0));
        }
        let mut model = fit(names, &values, neurons, deadline)?;
        (model.error_median, model.error_spread) = usual(errors, &model.means, &model.scales);
        Ok(Some(model))
    }

    /// How `statistics` compare with their reconstruction.
    #[must_use]
    pub fn reconstruct(&self, statistics: &Statistics) -> Reconstruction {
        compare(
            (&self.names, &self.means, &self.scales),
            statistics,
            |values| self.error_of(values),
        )
    }

    fn error_of(&self, values: &[f64]) -> (f64, Vec<f64>) {
        let network: Network = self
            .layers
            .iter()
            .map(|layer| {
                (
                    DMatrix::from_row_slice(layer.outputs, layer.inputs, &layer.weights),
                    DVector::from_column_slice(&layer.biases),
                )
            })
            .collect();
        let standard = DVector::from_vec(standardised(values, &self.means, &self.scales));
        let input = DMatrix::from_column_slice(standard.len(), 1, standard.as_slice());
        let rebuilt = forward(&network, &input).output.column(0).into_owned();
        error_and_values(&standard, &rebuilt, &self.means, &self.scales)
    }
}

/// The model of `values` (one row per episode), before its errors are
/// learned.
fn fit(
    names: Vec<String>,
    values: &[Vec<f64>],
    neurons: usize,
    deadline: Instant,
) -> Result<NeuralAutoencoder, TimeLimit> {
    let columns = names.len();
    let (means, scales) = standardisation(values, columns);
    let inputs = DMatrix::from_fn(columns, values.len(), |row, column| {
        (values[column][row] - means[row]) / scales[row]
    });
    let code = (columns / 4).clamp(1, 8);
    let (network, epochs) = learn(&inputs, neurons.max(1), code, deadline)?;
    let layers = network
        .into_iter()
        .map(|(weights, biases)| Layer {
            inputs: weights.ncols(),
            outputs: weights.nrows(),
            weights: weights.transpose().as_slice().to_vec(),
            biases: biases.as_slice().to_vec(),
        })
        .collect();
    Ok(NeuralAutoencoder {
        names,
        means,
        scales,
        layers,
        episodes: values.len(),
        epochs,
        error_median: 0.0,
        error_spread: 0.0,
    })
}

/// Every layer's output for `inputs`, one column per episode.
struct Pass {
    hidden_in: DMatrix<f64>,
    code: DMatrix<f64>,
    hidden_out: DMatrix<f64>,
    output: DMatrix<f64>,
}

fn forward(network: &Network, inputs: &DMatrix<f64>) -> Pass {
    let layer = |index: usize, inputs: &DMatrix<f64>| {
        let (weights, biases) = &network[index];
        let mut outputs = weights * inputs;
        for mut column in outputs.column_iter_mut() {
            column += biases;
        }
        outputs
    };
    let hidden_in = layer(0, inputs).map(f64::tanh);
    let code = layer(1, &hidden_in);
    let hidden_out = layer(2, &code).map(f64::tanh);
    let output = layer(3, &hidden_out);
    Pass {
        hidden_in,
        code,
        hidden_out,
        output,
    }
}

/// Learns a network reconstructing `inputs` (one column per episode) by
/// Adam over all of them at once, and the passes it took.
fn learn(
    inputs: &DMatrix<f64>,
    neurons: usize,
    code: usize,
    deadline: Instant,
) -> Result<(Network, usize), TimeLimit> {
    let statistics = inputs.nrows();
    let mut seed = Seed(SEED);
    let mut network: Network = [
        (neurons, statistics),
        (code, neurons),
        (neurons, code),
        (statistics, neurons),
    ]
    .into_iter()
    .map(|(outputs, inputs)| {
        // Glorot's uniform range, which keeps tanh layers from saturating.
        #[allow(clippy::cast_precision_loss, reason = "layer widths are small")]
        let limit = (6.0 / (outputs + inputs) as f64).sqrt();
        (
            DMatrix::from_fn(outputs, inputs, |_, _| seed.between(-limit, limit)),
            DVector::zeros(outputs),
        )
    })
    .collect();
    let mut first: Network = network
        .iter()
        .map(|(w, b)| (w.map(|_| 0.0), b.map(|_| 0.0)))
        .collect();
    let mut second = first.clone();
    #[allow(clippy::cast_precision_loss, reason = "counts are small")]
    let values = (inputs.len()) as f64;
    let (mut best, mut since) = (f64::INFINITY, 0);
    for epoch in 1..=MAX_EPOCHS {
        if Instant::now() >= deadline {
            return Err(TimeLimit);
        }
        let pass = forward(&network, inputs);
        let difference = &pass.output - inputs;
        let error = difference.norm_squared() / values;
        if error < best * (1.0 - 1e-4) {
            (best, since) = (error, 0);
        } else {
            since += 1;
            if since >= PATIENCE {
                return Ok((network, epoch));
            }
        }
        // Backpropagation of the mean squared error.
        let d_output = difference * (2.0 / values);
        let d_hidden_out = (network[3].0.transpose() * &d_output)
            .component_mul(&pass.hidden_out.map(|a| 1.0 - a * a));
        let d_code = network[2].0.transpose() * &d_hidden_out;
        let d_hidden_in = (network[1].0.transpose() * &d_code)
            .component_mul(&pass.hidden_in.map(|a| 1.0 - a * a));
        let gradients = [
            (&d_hidden_in * inputs.transpose(), d_hidden_in.column_sum()),
            (&d_code * pass.hidden_in.transpose(), d_code.column_sum()),
            (
                &d_hidden_out * pass.code.transpose(),
                d_hidden_out.column_sum(),
            ),
            (
                &d_output * pass.hidden_out.transpose(),
                d_output.column_sum(),
            ),
        ];
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            reason = "at most MAX_EPOCHS"
        )]
        let corrections = (
            1.0 - DECAY.0.powi(epoch as i32),
            1.0 - DECAY.1.powi(epoch as i32),
        );
        for (index, (weights, biases)) in gradients.into_iter().enumerate() {
            adam(
                &mut network[index].0,
                &mut first[index].0,
                &mut second[index].0,
                &weights,
                corrections,
            );
            adam(
                &mut network[index].1,
                &mut first[index].1,
                &mut second[index].1,
                &biases,
                corrections,
            );
        }
    }
    Ok((network, MAX_EPOCHS))
}

/// One Adam step of `parameters` along `gradient`, keeping its running
/// averages of the gradient and its square.
fn adam<R: nalgebra::Dim, C: nalgebra::Dim>(
    parameters: &mut nalgebra::OMatrix<f64, R, C>,
    first: &mut nalgebra::OMatrix<f64, R, C>,
    second: &mut nalgebra::OMatrix<f64, R, C>,
    gradient: &nalgebra::OMatrix<f64, R, C>,
    (first_correction, second_correction): (f64, f64),
) where
    nalgebra::DefaultAllocator: nalgebra::allocator::Allocator<R, C>,
{
    first.zip_apply(gradient, |average, g| {
        *average = DECAY.0 * *average + (1.0 - DECAY.0) * g;
    });
    second.zip_apply(gradient, |average, g| {
        *average = DECAY.1 * *average + (1.0 - DECAY.1) * g * g;
    });
    parameters.zip_zip_apply(first, second, |parameter, m, v| {
        *parameter -= STEP * (m / first_correction) / ((v / second_correction).sqrt() + 1e-8);
    });
}

/// `SplitMix64`: the same numbers from the same seed, on every machine.
struct Seed(u64);

impl Seed {
    /// The next number, uniform between `low` and `high`.
    fn between(&mut self, low: f64, high: f64) -> f64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        #[allow(clippy::cast_precision_loss, reason = "53 bits fit an f64 exactly")]
        let unit = (z >> 11) as f64 / (1_u64 << 53) as f64;
        low + (high - low) * unit
    }
}
