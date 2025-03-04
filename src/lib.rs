#![no_std]

extern crate alloc;
use alloc::{vec, vec::Vec};
use rand_core::RngCore;
use sample_consensus::{Consensus, Estimator, Model};

/// The ARRSAC algorithm for sample consensus.
pub struct Arrsac<R> {
    max_candidate_hypotheses: usize,
    block_size: usize,
    max_delta_estimations: usize,
    likelyhood_ratio_threshold: f32,
    initial_epsilon: f32,
    initial_delta: f32,
    inlier_threshold: f64,
    rng: R,
    random_samples: Vec<u32>,
}

impl<R> Arrsac<R>
where
    R: RngCore,
{
    /// `rng` should have the same properties you would want for a Monte Carlo simulation.
    /// It should generate random numbers quickly without having any discernable patterns.
    ///
    /// The `inlier_threshold` is the one parameter that is always specific to your dataset.
    /// This must be set to the threshold in which a data point's residual is considered an inlier.
    /// Some of the other parameters may need to be configured based on the amount of data,
    /// such as `block_size`, `likelyhood_ratio_threshold`, and `block_size`. However,
    /// `inlier_threshold` has to be set based on the residual function used with the model.
    pub fn new(inlier_threshold: f64, rng: R) -> Self {
        Self {
            max_candidate_hypotheses: 50,
            block_size: 100,
            max_delta_estimations: 4,
            likelyhood_ratio_threshold: 1e6,
            initial_epsilon: 0.1,
            initial_delta: 0.05,
            inlier_threshold,
            rng,
            random_samples: vec![],
        }
    }

    /// Number of hypotheses that will be generated for each block of data evaluated
    ///
    /// Default: `50`
    pub fn max_candidate_hypotheses(self, max_candidate_hypotheses: usize) -> Self {
        Self {
            max_candidate_hypotheses,
            ..self
        }
    }

    /// Number of data points evaluated before more hypotheses are generated
    ///
    /// Default: `100`
    pub fn block_size(self, block_size: usize) -> Self {
        Self { block_size, ..self }
    }

    /// Number of times that the entire dataset is compared against a bad model to see
    /// the probability of inliers in a bad model
    ///
    /// Default: `4`
    pub fn max_delta_estimations(self, max_delta_estimations: usize) -> Self {
        Self {
            max_delta_estimations,
            ..self
        }
    }

    /// Once a model reaches this level of unlikelyhood, it is rejected. Set this
    /// higher to make it less restrictive, usually at the cost of more execution time.
    ///
    /// Increasing this will make it more likely to find a good result (unless it is set very high).
    ///
    /// Decreasing this will speed up execution.
    ///
    /// This ratio is not exposed as a parameter in the original paper, but is instead computed
    /// recursively for a few iterations. It is roughly equivalent to the **reciprocal** of the
    /// **probability of rejecting a good model**. You can use that to control the probability
    /// that a good model is rejected.
    ///
    /// Default: `1e6`
    pub fn likelyhood_ratio_threshold(self, likelyhood_ratio_threshold: f32) -> Self {
        Self {
            likelyhood_ratio_threshold,
            ..self
        }
    }

    /// Initial anticipated probability of an inlier being part of a good model
    ///
    /// This is an estimation that will be updated as ARRSAC executes. The initial
    /// estimate is purposefully low, which will accept more models. As models are
    /// accepted, it will gradually increase it to match the best model found so far,
    /// which makes it more restrictive.
    ///
    /// Default: `0.1`
    pub fn initial_epsilon(self, initial_epsilon: f32) -> Self {
        Self {
            initial_epsilon,
            ..self
        }
    }

    /// Initial anticipated probability of an inlier being part of a bad model
    ///
    /// This is an estimation that will be updated as ARRSAC executes. The initial
    /// estimate is almost certainly incorrect. This can be modified for different data
    /// to get better/faster results. As models are rejected, it will update this value
    /// until it has evaluated it `max_delta_estimations` times.
    ///
    /// Default: `0.05`
    pub fn initial_delta(self, initial_delta: f32) -> Self {
        Self {
            initial_delta,
            ..self
        }
    }

    /// Residual threshold for determining if a data point is an inlier or an outlier of a model
    pub fn inlier_threshold(self, inlier_threshold: f64) -> Self {
        Self {
            inlier_threshold,
            ..self
        }
    }

    /// Algorithm 3 from "A Comparative Analysis of RANSAC Techniques Leading to Adaptive Real-Time Random Sample Consensus"
    ///
    /// At least at present, this does not use the PROSAC method and instead does completely random sampling.
    ///
    /// Returns the initial models (and their num inliers), `epsilon`, and `delta` in that order.
    fn initial_hypotheses<E, Data>(
        &mut self,
        estimator: &E,
        data: impl Iterator<Item = Data> + Clone,
    ) -> (Vec<(E::Model, usize)>, f32, f32)
    where
        E: Estimator<Data>,
    {
        let mut hypotheses = vec![];
        // We don't want more than `block_size` data points to be used to evaluate models initially.
        let initial_datapoints = core::cmp::min(self.block_size, data.clone().count());
        // Set the best inliers to be the floor of what the number of inliers would need to be to be the initial epsilon.
        let mut best_inliers =
            libm::floorf(self.initial_epsilon * initial_datapoints as f32) as usize;
        // Set the initial epsilon (inlier ratio in good model).
        let mut epsilon = self.initial_epsilon;
        // Set the initial delta (outlier ratio in good model).
        let mut delta = self.initial_delta;
        let mut positive_likelyhood_ratio = delta / epsilon;
        let mut negative_likelyhood_ratio = (1.0 - delta) / (1.0 - epsilon);
        let mut current_delta_estimations = 0;
        let mut total_delta_inliers = 0;
        let mut best_inlier_indices = vec![];
        let mut random_hypotheses = vec![];
        // Lets us know if we found a candidate hypothesis that actually has enough inliers for us to generate a model from.
        let mut found_usable_hypothesis = false;
        // Iterate through all the randomly generated hypotheses to update epsilon and delta while finding good models.
        for _ in 0..self.max_candidate_hypotheses {
            if found_usable_hypothesis {
                // If we have found a hypothesis that has a sufficient number of inliers, we randomly sample from its inliers
                // to generate new hypotheses since that is much more likely to generate good ones.
                random_hypotheses.extend(self.generate_random_hypotheses_subset(
                    estimator,
                    data.clone(),
                    &best_inlier_indices,
                ));
            } else {
                // Generate the random hypotheses using all the data, not just the evaluation data.
                random_hypotheses.extend(self.generate_random_hypotheses(estimator, data.clone()));
            }
            for model in random_hypotheses.drain(..) {
                // Check if the model satisfies the ASPRT test on only `inital_datapoints` evaluation data.
                let (pass, inliers) = self.asprt(
                    data.clone().take(initial_datapoints),
                    &model,
                    positive_likelyhood_ratio,
                    negative_likelyhood_ratio,
                );
                if pass {
                    // If this has the largest support (most inliers) then we update the
                    // approximation of epsilon.
                    if inliers > best_inliers {
                        best_inliers = inliers;
                        // Update epsilon (this can only increase, since there are more inliers).
                        epsilon = inliers as f32 / data.clone().count() as f32;
                        // Will decrease positive likelyhood ratio.
                        positive_likelyhood_ratio = delta / epsilon;
                        // Will increase negative likelyhood ratio.
                        negative_likelyhood_ratio = (1.0 - delta) / (1.0 - epsilon);

                        // We only want to mark the hypothesis as usable if the inliers can generate a model.
                        // Some models might be incredibly low on inliers and we can't accept them.
                        if inliers > E::MIN_SAMPLES {
                            // Update the inlier indices appropriately.
                            best_inlier_indices = self.inliers(data.clone(), &model);
                            // Mark that a usable hypothesis has been found.
                            found_usable_hypothesis = true;
                        }
                    }
                    hypotheses.push((model, inliers));
                } else if current_delta_estimations < self.max_delta_estimations {
                    // We want to add the information about inliers to our estimation of delta.
                    // We only do this up to `max_delta_estimations` times to avoid wasting too much time.
                    total_delta_inliers += self.count_inliers(data.clone(), &model);
                    current_delta_estimations += 1;
                    // Update delta.
                    delta = total_delta_inliers as f32
                        / (current_delta_estimations * data.clone().count()) as f32;
                    // May change positive likelyhood ratio.
                    positive_likelyhood_ratio = delta / epsilon;
                    // May change negative likelyhood ratio.
                    negative_likelyhood_ratio = (1.0 - delta) / (1.0 - epsilon);
                }
            }
        }

        (hypotheses, epsilon, delta)
    }

    /// Populates `self.random_samples` using a len.
    fn populate_samples(&mut self, num: usize, len: usize) {
        // We can generate no hypotheses if the amout of data is too low.
        if len < num {
            panic!("cannot use arrsac without having enough samples");
        }
        let len = len as u32;
        // Threshold generation below adapted from randomize::RandRangeU32.
        let threshold = len.wrapping_neg() % len;
        self.random_samples.clear();
        for _ in 0..num {
            loop {
                let mul = u64::from(self.rng.next_u32()).wrapping_mul(u64::from(len));
                if mul as u32 >= threshold {
                    let s = (mul >> 32) as u32;
                    if !self.random_samples.contains(&s) {
                        self.random_samples.push(s);
                        break;
                    }
                }
            }
        }
    }

    /// Generates as many hypotheses as one call to `Estimator::estimate()` returns from all data.
    fn generate_random_hypotheses<E, Data>(
        &mut self,
        estimator: &E,
        data: impl Iterator<Item = Data> + Clone,
    ) -> E::ModelIter
    where
        E: Estimator<Data>,
    {
        self.populate_samples(E::MIN_SAMPLES, data.clone().count());
        estimator.estimate(
            self.random_samples
                .iter()
                .map(|&ix| data.clone().nth(ix as usize).unwrap()),
        )
    }

    /// Generates as many hypotheses as one call to `Estimator::estimate()` returns from a subset of the data.
    fn generate_random_hypotheses_subset<E, Data>(
        &mut self,
        estimator: &E,
        data: impl Iterator<Item = Data> + Clone,
        subset: &[usize],
    ) -> E::ModelIter
    where
        E: Estimator<Data>,
    {
        self.populate_samples(E::MIN_SAMPLES, subset.len());
        estimator.estimate(
            core::mem::replace(&mut self.random_samples, vec![])
                .iter()
                .map(|&ix| data.clone().nth(subset[ix as usize]).unwrap()),
        )
    }

    /// Algorithm 1 in "Randomized RANSAC with Sequential Probability Ratio Test".
    ///
    /// This tests if a model is accepted. Returns `true` on accepted and `false` on rejected.
    ///
    /// `inlier_threshold` - The model residual error threshold between inliers and outliers
    /// `positive_likelyhood_ratio` - `δ / ε`
    /// `negative_likelyhood_ratio` - `(1 - δ) / (1 - ε)`
    fn asprt<Data, M: Model<Data>>(
        &self,
        data: impl Iterator<Item = Data>,
        model: &M,
        positive_likelyhood_ratio: f32,
        negative_likelyhood_ratio: f32,
    ) -> (bool, usize) {
        let mut likelyhood_ratio = 1.0;
        let mut inliers = 0;
        for data in data {
            likelyhood_ratio *= if model.residual(&data) < self.inlier_threshold {
                inliers += 1;
                positive_likelyhood_ratio
            } else {
                negative_likelyhood_ratio
            };

            if likelyhood_ratio > self.likelyhood_ratio_threshold {
                return (false, 0);
            }
        }

        (true, inliers)
    }

    /// This function sorts and retains the correct number of hypotheses when evaluating data item `i`.
    fn retain_hypotheses<M>(&self, item: usize, hypotheses: &mut Vec<(M, usize)>) {
        // TODO: See if there is some way to re-write this to include no floating-point math.
        // TODO: I am going against what the paper says by using max here instead of min,
        // but with min this makes absolutely no sense since in a block size of 100
        // it will be guaranteed to terminate because log2(initial_hypotheses) << 100.
        // I am making an executive decision to assume that this is a max instead of a min.
        let num_retain = core::cmp::min(
            hypotheses.len(),
            core::cmp::max(
                hypotheses.len() / 2,
                (self.max_candidate_hypotheses as f32
                    * libm::powf(2.0f32, -(item as f32) / self.block_size as f32))
                    as usize,
            ),
        );
        // We need to sort the hypotheses based on how good they are (number inliers).
        // The best hypotheses go to the beginning.
        hypotheses.sort_unstable_by_key(|&(_, inliers)| -(inliers as isize));
        hypotheses.resize_with(num_retain, || {
            panic!("Arrsac::models should never resize the hypotheses to be higher");
        });
    }

    /// Determines the number of inliers a model has.
    fn count_inliers<Data, M: Model<Data>>(
        &self,
        data: impl Iterator<Item = Data>,
        model: &M,
    ) -> usize {
        data.filter(|data| model.residual(data) < self.inlier_threshold)
            .count()
    }

    /// Gets indices of inliers for a model.
    fn inliers<Data, M: Model<Data>>(
        &self,
        data: impl Iterator<Item = Data>,
        model: &M,
    ) -> Vec<usize> {
        data.enumerate()
            .filter(|(_, data)| model.residual(data) < self.inlier_threshold)
            .map(|(ix, _)| ix)
            .collect()
    }
}

impl<E, R, Data> Consensus<E, Data> for Arrsac<R>
where
    E: Estimator<Data>,
    R: RngCore,
{
    type Inliers = Vec<usize>;

    fn model<I>(&mut self, estimator: &E, data: I) -> Option<E::Model>
    where
        I: Iterator<Item = Data> + Clone,
    {
        self.model_inliers(estimator, data).map(|(model, _)| model)
    }

    fn model_inliers<I>(&mut self, estimator: &E, data: I) -> Option<(E::Model, Self::Inliers)>
    where
        I: Iterator<Item = Data> + Clone,
    {
        // Don't do anything if we don't have enough data.
        if data.clone().count() < E::MIN_SAMPLES {
            return None;
        }
        // Generate the initial set of hypotheses. This also gets us an estimate of epsilon and delta.
        // We only want to give it one block size of data for the initial generation.
        let (mut hypotheses, _, delta) = self.initial_hypotheses(estimator, data.clone());

        let mut random_hypotheses = Vec::new();

        // Retain the hypotheses the initial time. This is done before the loop to ensure that if the
        // number of datapoints is too low and the for loop never executes that the best model is returned.
        self.retain_hypotheses(self.block_size, &mut hypotheses);

        // If there are no initial hypotheses then don't bother doing anything.
        if hypotheses.is_empty() {
            return None;
        }

        // Gradually increase how many datapoints we are evaluating until we evaluate them all.
        for num_data in self.block_size + 1..=data.clone().count() {
            if hypotheses.len() <= 1 {
                break;
            }
            // Score the hypotheses with the new datapoint.
            let new_datapoint = &data.clone().nth(num_data - 1).unwrap();
            for (hypothesis, inlier_count) in hypotheses.iter_mut() {
                if hypothesis.residual(new_datapoint) < self.inlier_threshold {
                    *inlier_count += 1;
                }
            }
            // Every block size we do this.
            if num_data % self.block_size == 0 {
                // First, update epsilon using the best model.
                // Technically model 0 might no longer be the best model after evaluating the last data-point,
                // but that is not that important.
                let epsilon = hypotheses[0].1 as f32 / num_data as f32;
                // Create the likelyhood ratios for inliers and outliers.
                let positive_likelyhood_ratio = delta / epsilon;
                let negative_likelyhood_ratio = (1.0 - delta) / (1.0 - epsilon);
                // Generate the list of inliers for the best model.
                let inliers = self.inliers(data.clone(), &hypotheses[0].0);
                // We generate hypotheses until we reach the initial num hypotheses.
                for _ in 0..self.max_candidate_hypotheses {
                    random_hypotheses.extend(self.generate_random_hypotheses_subset(
                        estimator,
                        data.clone(),
                        &inliers,
                    ));
                    for model in random_hypotheses.drain(..) {
                        let (pass, inliers) = self.asprt(
                            data.clone().take(num_data),
                            &model,
                            positive_likelyhood_ratio,
                            negative_likelyhood_ratio,
                        );
                        if pass {
                            hypotheses.push((model, inliers));
                        }
                    }
                }
            }
            // This will retain at least half of the hypotheses each time
            // and gradually decrease as the number of samples we are evaluating increases.
            self.retain_hypotheses(num_data, &mut hypotheses);
        }
        hypotheses.into_iter().next().map(|(model, _)| {
            let inliers = self.inliers(data.clone(), &model);
            (model, inliers)
        })
    }
}
