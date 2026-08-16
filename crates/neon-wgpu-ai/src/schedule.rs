//! Cosine noise schedule for the terrain DDPM, mirroring `betas_cosine` in
//! `assets/ai/terrain_run1/train.py`. All math stays in f32 to match PyTorch
//! tensor arithmetic closely enough for golden comparison tolerances.

/// Cosine schedule beta values (length `T`), computed in f32 exactly like
/// `torch.cos(...) ** 2` with `torch.clip(..., max=0.999)`.
pub fn betas_cosine_f32(T: u32, s: f64) -> Vec<f32> {
    let t = T as usize;
    let mut alphas = Vec::with_capacity(t);
    let mut betas = Vec::with_capacity(t);
    for i in 0..t {
        let step = i as f32 / T as f32;
        let step1 = (i + 1) as f32 / T as f32;
        let c0 = ((step + s as f32) / (1.0 + s as f32) * std::f32::consts::PI / 2.0).cos();
        let c1 = ((step1 + s as f32) / (1.0 + s as f32) * std::f32::consts::PI / 2.0).cos();
        alphas.push(c0 * c0);
        let b = 1.0 - c1 * c1 / (c0 * c0).max(1e-12);
        betas.push(b.min(0.999));
    }
    let _ = alphas;
    betas
}

/// Precomputed schedule arrays used by the sampler.
#[derive(Clone, Debug)]
pub struct Schedule {
    pub T: u32,
    pub sab: Vec<f32>,
    pub s1ab: Vec<f32>,
}

impl Schedule {
    /// Mirrors the training-loop schedule construction (cosine, s = 0.008).
    pub fn cosine(T: u32) -> Self {
        let betas = betas_cosine_f32(T, 0.008);
        let mut alpha_bar = vec![0.0_f32; T as usize];
        let mut acc = 1.0_f32;
        let mut sab = vec![0.0_f32; T as usize];
        let mut s1ab = vec![0.0_f32; T as usize];
        for (i, beta) in betas.iter().enumerate() {
            acc *= 1.0 - beta;
            alpha_bar[i] = acc;
            sab[i] = acc.sqrt();
            s1ab[i] = (1.0 - acc).sqrt();
        }
        let _ = alpha_bar;
        Self { T, sab, s1ab }
    }

    /// DDIM timestep mapping identical to the Python loop:
    /// `t1 = int((T - 1) * (1 - i / steps)); t0 = int((T - 1) * (1 - (i + 1) / steps))`.
    #[must_use]
    pub fn ddim_times(&self, steps: u32, i: u32) -> (u32, u32) {
        let t1 = (((self.T - 1) as f64) * (1.0 - i as f64 / steps as f64)) as u32;
        let t0 = (((self.T - 1) as f64) * (1.0 - (i + 1) as f64 / steps as f64)) as u32;
        (t1.max(1), t0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hardcoded golden values produced by the pure-Python cosine schedule at
    /// the same f32 rounding (see `assets/ai/terrain_run1` notes).
    #[test]
    fn cosine_schedule_matches_python_reference() {
        let s = Schedule::cosine(1000);
        assert_eq!(s.T, 1000);
        assert_eq!(s.sab.len(), 1000);
        assert_eq!(s.s1ab.len(), 1000);
        // Endpoints: the very first timestep is near-0 noise, the last is full noise.
        assert!(s.sab[0] > 0.999, "first step keeps the signal");
        assert!(s.sab[999] < 0.05, "last step is near-pure noise");
        assert!(s.s1ab[0] < 0.02, "first step carries almost no noise");
        assert!(s.s1ab[999] > 0.995, "last step carries almost all noise");
        // Monotonic structure.
        for window in s.sab.windows(2) {
            assert!(window[0] >= window[1], "alpha_bar must be non-increasing");
        }
        // Exact probe values captured from the pure-Python reference (f64);
        // the f32 GPU schedule must stay inside the relative tolerance.
        let expect = [
            (0usize, 0.999_979_36f32),
            (100, 0.985_685_4),
            (200, 0.947_503_5),
            (300, 0.886_359_1),
            (400, 0.803_733_9),
            (500, 0.701_630_4),
            (600, 0.582_523_0),
            (700, 0.449_298_2),
            (800, 0.305_184_8),
            (900, 0.153_675_34),
            (999, 4.928_252e-5),
        ];
        for (i, v) in expect {
            let got = s.sab[i];
            assert!(
                (got - v).abs() <= v * 5e-4 + 1e-6,
                "sab[{i}] = {got} (expected {v})"
            );
        }
    }

    #[test]
    fn ddim_times_match_python_truncation() {
        let s = Schedule::cosine(1000);
        assert_eq!(s.ddim_times(50, 0), (999, 979));
        assert_eq!(s.ddim_times(50, 1), (979, 959));
        assert_eq!(s.ddim_times(50, 25), (499, 479));
        assert_eq!(s.ddim_times(50, 49), (19, 0));
        assert_eq!(s.ddim_times(20, 0), (999, 949));
        assert_eq!(s.ddim_times(20, 19), (49, 0));
    }
}
