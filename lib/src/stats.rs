//! Summary statistics over a sample of values (shot-group range bars).

/// Summary of a sample of finite values. Empty or all-non-finite input gives
/// `None` from [`Stats::of`].
///
/// Unit-agnostic: pass values already converted to the display units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    /// Number of finite values in the sample.
    pub count: usize,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    /// Middle value; the mean of the two middle values when `count` is even.
    pub median: f64,
    /// Sample standard deviation (n − 1). `None` when `count < 2`.
    pub std_dev: Option<f64>,
}

impl Stats {
    /// Summarise `values`, dropping NaN and infinities. `None` when no finite
    /// value remains.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // sample sizes are far below 2^52
    pub fn of(values: impl IntoIterator<Item = f64>) -> Option<Stats> {
        let mut sorted: Vec<f64> = values.into_iter().filter(|v| v.is_finite()).collect();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(f64::total_cmp);
        let count = sorted.len();
        let n = count as f64;
        let mean = sorted.iter().sum::<f64>() / n;
        let mid = count / 2;
        let median = if count.is_multiple_of(2) {
            f64::midpoint(sorted[mid - 1], sorted[mid])
        } else {
            sorted[mid]
        };
        let std_dev = (count >= 2).then(|| {
            let ss: f64 = sorted.iter().map(|v| (v - mean).powi(2)).sum();
            (ss / (n - 1.0)).sqrt()
        });
        Some(Stats {
            count,
            min: sorted[0],
            max: sorted[count - 1],
            mean,
            median,
            std_dev,
        })
    }

    /// `max - min`.
    #[must_use]
    pub fn range(&self) -> f64 {
        self.max - self.min
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(Stats::of(Vec::<f64>::new()), None);
        assert_eq!(
            Stats::of([f64::NAN, f64::INFINITY, f64::NEG_INFINITY]),
            None
        );
    }

    #[test]
    fn one_value() {
        let s = Stats::of([232.0]).expect("stats");
        assert_eq!(s.count, 1);
        assert!(close(s.min, 232.0));
        assert!(close(s.max, 232.0));
        assert!(close(s.mean, 232.0));
        assert!(close(s.median, 232.0));
        assert_eq!(s.std_dev, None);
        assert!(close(s.range(), 0.0));
    }

    #[test]
    fn odd_median() {
        let s = Stats::of([9.0, 1.0, 5.0]).expect("stats");
        assert!(close(s.median, 5.0));
        assert!(close(s.min, 1.0));
        assert!(close(s.max, 9.0));
        assert!(close(s.range(), 8.0));
    }

    #[test]
    fn even_median_is_mean_of_middle_two() {
        let s = Stats::of([4.0, 1.0, 3.0, 10.0]).expect("stats");
        assert!(close(s.median, 3.5));
        assert!(close(s.mean, 4.5));
    }

    #[test]
    fn non_finite_values_are_dropped() {
        let s = Stats::of([1.0, f64::NAN, 3.0, f64::INFINITY]).expect("stats");
        assert_eq!(s.count, 2);
        assert!(close(s.mean, 2.0));
        assert!(close(s.max, 3.0));
    }

    #[test]
    fn known_sample_std_dev() {
        let s = Stats::of([2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]).expect("stats");
        assert_eq!(s.count, 8);
        assert!(close(s.mean, 5.0));
        // Sum of squares 32 over n - 1 = 7.
        let sd = s.std_dev.expect("sd");
        assert!(close(sd, (32.0_f64 / 7.0).sqrt()));
        assert!((sd - 2.138).abs() < 1e-3);
    }

    #[test]
    fn negative_values() {
        let s = Stats::of([-3.5, 2.0, -1.0]).expect("stats");
        assert!(close(s.min, -3.5));
        assert!(close(s.max, 2.0));
        assert!(close(s.median, -1.0));
        assert!(close(s.mean, -2.5 / 3.0));
        assert!(close(s.range(), 5.5));
    }
}
