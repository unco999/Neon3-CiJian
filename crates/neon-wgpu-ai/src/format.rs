//! Binary weight-pack format (`NEONWAI1`) and the terrain UNet model spec.
//!
//! The pack is produced offline by `assets/ai/terrain_run1/convert_ckpt.py`
//! from the PyTorch checkpoint and validated here with strict schema checks so
//! a mismatched model can never be loaded silently.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::AiError;

pub const MAGIC: &[u8; 8] = b"NEONWAI1";
pub const FORMAT_VERSION: u32 = 1;
pub const DTYPE_F32: u32 = 0;

pub const MODEL_KIND_TERRAIN_UNET_DDIM_V1: &str = "terrain_unet_ddim_v1";
pub const MODEL_NAME: &str = MODEL_KIND_TERRAIN_UNET_DDIM_V1;

/// Embedding index tables, mirrored from `train.py`. `None` means the null
/// (unconditional) class, which maps to index `len(classes)`.
pub const SUB_CLASSES: [&str; 23] = [
    "alpine",
    "caldera_rim",
    "canyon_gorge",
    "cliff_coast",
    "delta",
    "dissected_hills",
    "dune_sea",
    "fjord",
    "flat_plain",
    "glacier_highland",
    "hamada",
    "high_plateau",
    "lava_plateau",
    "mesa_badlands",
    "mid_mountain",
    "rocky_wadi",
    "rolling_hills",
    "salt_playa",
    "sandy_coast",
    "shield_volcano",
    "stratovolcano",
    "tundra_lowland",
    "undulating_plain",
];
pub const PARENT_CLASSES: [&str; 8] = [
    "coastal", "desert", "glacial", "hill", "mountain", "plain", "plateau", "volcanic",
];
pub const RELIEF_CLASSES: [&str; 5] = ["flat", "low", "mid", "high", "extreme"];
pub const TEXTURE_CLASSES: [&str; 4] = ["smooth", "undulating", "fine_ridged", "coarse_rugged"];
pub const WATER_CLASSES: [&str; 3] = ["land", "water_edge", "water_lots"];

/// Fully-specified terrain generation condition. Each field is a class index or
/// `None` (null / unconditional class).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TerrainCond {
    pub sub: Option<u32>,
    pub parent: Option<u32>,
    pub relief: Option<u32>,
    pub texture: Option<u32>,
    pub water: Option<u32>,
}

impl TerrainCond {
    /// Look up a class index by name; returns the null index for `None`.
    pub fn indices(&self) -> [u32; 5] {
        [
            self.sub.unwrap_or(SUB_CLASSES.len() as u32),
            self.parent.unwrap_or(PARENT_CLASSES.len() as u32),
            self.relief.unwrap_or(RELIEF_CLASSES.len() as u32),
            self.texture.unwrap_or(TEXTURE_CLASSES.len() as u32),
            self.water.unwrap_or(WATER_CLASSES.len() as u32),
        ]
    }

    /// Class index lookup with bounds validation against the class tables.
    #[must_use]
    pub fn validate(self) -> Result<Self, AiError> {
        fn check(name: &str, value: Option<u32>, len: usize) -> Result<Option<u32>, AiError> {
            match value {
                None => Ok(None),
                Some(index) if (index as usize) < len => Ok(Some(index)),
                Some(index) => Err(AiError::InvalidRequest(format!(
                    "{name} class index {index} is out of range (0..{len})"
                ))),
            }
        }
        Ok(Self {
            sub: check("sub", self.sub, SUB_CLASSES.len())?,
            parent: check("parent", self.parent, PARENT_CLASSES.len())?,
            relief: check("relief", self.relief, RELIEF_CLASSES.len())?,
            texture: check("texture", self.texture, TEXTURE_CLASSES.len())?,
            water: check("water", self.water, WATER_CLASSES.len())?,
        })
    }

    #[must_use]
    pub fn from_indices(indices: [u32; 5]) -> Self {
        Self {
            sub: ((indices[0] as usize) < SUB_CLASSES.len()).then_some(indices[0]),
            parent: ((indices[1] as usize) < PARENT_CLASSES.len()).then_some(indices[1]),
            relief: ((indices[2] as usize) < RELIEF_CLASSES.len()).then_some(indices[2]),
            texture: ((indices[3] as usize) < TEXTURE_CLASSES.len()).then_some(indices[3]),
            water: ((indices[4] as usize) < WATER_CLASSES.len()).then_some(indices[4]),
        }
    }
}

/// Metadata carried in the pack header.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackMeta {
    pub model_kind: String,
    pub dtype: String,
    pub T: u32,
    pub base: u32,
    pub schedule: String,
    pub source_ckpt: String,
    pub param_count: u64,
    pub sha256: String,
    pub created_at: String,
}

/// One named tensor inside a pack; data is a view into the pack bytes.
#[derive(Clone, Debug)]
pub struct TensorRef<'a> {
    pub name: String,
    pub dims: Vec<u32>,
    pub bytes: &'a [u8],
}

impl TensorRef<'_> {
    /// Copy the payload as little-endian f32. The pack bytes are not
    /// guaranteed to be 4-byte aligned, so this never casts in place.
    #[must_use]
    pub fn to_f32(&self) -> Result<Vec<f32>, AiError> {
        if self.bytes.len() % 4 != 0 {
            return Err(AiError::InvalidPack(format!(
                "tensor {} has non-f32 byte length",
                self.name
            )));
        }
        let mut out = Vec::with_capacity(self.bytes.len() / 4);
        for chunk in self.bytes.chunks_exact(4) {
            out.push(f32::from_le_bytes(chunk.try_into().unwrap()));
        }
        Ok(out)
    }

    #[must_use]
    pub fn numel(&self) -> u64 {
        self.dims.iter().map(|d| *d as u64).product()
    }

    /// Exact-shape check used by the structural validator.
    pub fn expect_dims(&self, dims: &[u32]) -> Result<(), AiError> {
        if self.dims == dims {
            Ok(())
        } else {
            Err(AiError::InvalidPack(format!(
                "tensor '{}' has dims {:?}, expected {:?}",
                self.name, self.dims, dims
            )))
        }
    }
}

/// Parsed pack: header + tensor directory over borrowed bytes.
pub struct WeightPack<'a> {
    pub meta: PackMeta,
    pub model_kind: u32,
    pub format_version: u32,
    pub dtype: u32,
    pub tensors: HashMap<String, TensorRef<'a>>,
}

impl<'a> WeightPack<'a> {
    /// Strictly parse a `NEONWAI1` pack.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, AiError> {
        let mut cursor = 0usize;
        let mut take = |n: usize, what: &str| -> Result<&'a [u8], AiError> {
            let end = cursor
                .checked_add(n)
                .ok_or_else(|| AiError::InvalidPack("size overflow".into()))?;
            if end > bytes.len() {
                return Err(AiError::InvalidPack(format!(
                    "truncated pack while reading {what}"
                )));
            }
            let slice = &bytes[cursor..end];
            cursor = end;
            Ok(slice)
        };

        let magic = take(8, "magic")?;
        if magic != MAGIC {
            return Err(AiError::InvalidPack(format!(
                "bad magic (expected {})",
                String::from_utf8_lossy(MAGIC)
            )));
        }
        let format_version = u32::from_le_bytes(take(4, "version")?.try_into().unwrap());
        if format_version != FORMAT_VERSION {
            return Err(AiError::InvalidPack(format!(
                "unsupported pack version {format_version}"
            )));
        }
        let model_kind = u32::from_le_bytes(take(4, "model kind")?.try_into().unwrap());
        if model_kind != 0 {
            return Err(AiError::InvalidPack(format!(
                "unsupported model kind {model_kind}"
            )));
        }
        let dtype = u32::from_le_bytes(take(4, "dtype")?.try_into().unwrap());
        if dtype != DTYPE_F32 {
            return Err(AiError::InvalidPack("only f32 packs are supported".into()));
        }
        let tensor_count =
            u32::from_le_bytes(take(4, "tensor count")?.try_into().unwrap()) as usize;
        let meta_len = u32::from_le_bytes(take(4, "meta length")?.try_into().unwrap()) as usize;
        let meta_bytes = take(meta_len, "meta")?;
        let meta: PackMeta = serde_json::from_slice(meta_bytes)
            .map_err(|error| AiError::InvalidPack(format!("meta is not valid JSON: {error}")))?;
        if meta.model_kind != MODEL_KIND_TERRAIN_UNET_DDIM_V1 {
            return Err(AiError::InvalidPack(format!(
                "pack is for model '{}', expected '{MODEL_KIND_TERRAIN_UNET_DDIM_V1}'",
                meta.model_kind
            )));
        }
        if meta.dtype != "f32" {
            return Err(AiError::InvalidPack(format!(
                "pack dtype '{}' is not supported",
                meta.dtype
            )));
        }

        let mut tensors = HashMap::with_capacity(tensor_count);
        for _ in 0..tensor_count {
            let name_len =
                u32::from_le_bytes(take(4, "tensor name length")?.try_into().unwrap()) as usize;
            let name_bytes = take(name_len, "tensor name")?;
            let name = std::str::from_utf8(name_bytes)
                .map_err(|_| AiError::InvalidPack("tensor name is not UTF-8".into()))?
                .to_owned();
            let dim_count =
                u32::from_le_bytes(take(4, "tensor dim count")?.try_into().unwrap()) as usize;
            if dim_count > 8 {
                return Err(AiError::InvalidPack(format!(
                    "tensor '{name}' has implausible rank {dim_count}"
                )));
            }
            let mut dims = Vec::with_capacity(dim_count);
            for _ in 0..dim_count {
                let d = u32::from_le_bytes(take(4, "tensor dim")?.try_into().unwrap());
                if d == 0 || d > (1 << 24) {
                    return Err(AiError::InvalidPack(format!(
                        "tensor '{name}' has implausible dim {d}"
                    )));
                }
                dims.push(d);
            }
            let byte_len =
                u32::from_le_bytes(take(4, "tensor byte length")?.try_into().unwrap()) as usize;
            let data = take(byte_len, &format!("tensor '{name}' data"))?;
            let numel = dims.iter().map(|d| *d as u64).product::<u64>() as usize;
            if numel * 4 != data.len() {
                return Err(AiError::InvalidPack(format!(
                    "tensor '{name}' dims {dims:?} do not match its {}-byte payload",
                    data.len()
                )));
            }
            if tensors
                .insert(
                    name.clone(),
                    TensorRef {
                        name,
                        dims,
                        bytes: data,
                    },
                )
                .is_some()
            {
                return Err(AiError::InvalidPack("duplicate tensor name in pack".into()));
            }
        }
        Ok(Self {
            meta,
            model_kind,
            format_version,
            dtype,
            tensors,
        })
    }

    pub fn tensor(&self, name: &str) -> Result<&TensorRef<'a>, AiError> {
        self.tensors
            .get(name)
            .ok_or_else(|| AiError::InvalidPack(format!("pack is missing tensor '{name}'")))
    }
}

/// Structural spec of the conditional terrain UNet, derived from `train.py`.
#[derive(Clone, Debug)]
pub struct TerrainUnetSpec {
    pub base: u32,
    pub ch_mults: [u32; 4],
    pub attn_from_level: usize,
    pub heads: u32,
    pub cond_dim: u32,
    pub time_dim: u32,
    pub group_groups: u32,
    pub input_ch: u32,
}

impl TerrainUnetSpec {
    pub const fn default_v1() -> Self {
        Self {
            base: 96,
            ch_mults: [1, 2, 4, 8],
            attn_from_level: 2,
            heads: 4,
            cond_dim: 256,
            time_dim: 256,
            group_groups: 8,
            input_ch: 1,
        }
    }

    #[must_use]
    pub fn channels(&self) -> [u32; 4] {
        [
            self.base * self.ch_mults[0],
            self.base * self.ch_mults[1],
            self.base * self.ch_mults[2],
            self.base * self.ch_mults[3],
        ]
    }

    /// Canonical (name, dims) layout of every weight the conditional terrain
    /// UNet executor reads. Both the pack validator and fixture generators
    /// are driven by this single list so they cannot drift apart.
    pub fn terrain_unet_layout(&self) -> Vec<(String, Vec<u32>)> {
        let cond_dim = self.cond_dim;
        let [c96, c192, c384, c768] = self.channels();
        let mut out = Vec::new();

        macro_rules! linear {
            ($name:expr, $out:expr, $in:expr) => {{
                out.push((format!("{}.weight", $name), vec![$out, $in]));
                out.push((format!("{}.bias", $name), vec![$out]));
            }};
        }
        macro_rules! conv {
            ($name:expr, $out:expr, $in:expr, $k:expr) => {{
                out.push((format!("{}.weight", $name), vec![$out, $in, $k, $k]));
                out.push((format!("{}.bias", $name), vec![$out]));
            }};
        }
        macro_rules! norm {
            ($name:expr, $c:expr) => {{
                out.push((format!("{}.weight", $name), vec![$c]));
                out.push((format!("{}.bias", $name), vec![$c]));
            }};
        }
        macro_rules! film {
            ($name:expr, $c:expr) => {{
                out.push((format!("{}.film.weight", $name), vec![$c * 2, cond_dim]));
                out.push((format!("{}.film.bias", $name), vec![$c * 2]));
            }};
        }
        macro_rules! res_block {
            ($name:expr, $cin:expr, $cout:expr, $skip:expr) => {{
                norm!(format!("{}.n1", $name), $cin);
                conv!(format!("{}.c1", $name), $cout, $cin, 3);
                norm!(format!("{}.n2", $name), $cout);
                conv!(format!("{}.c2", $name), $cout, $cout, 3);
                film!($name, $cout);
                if $skip {
                    out.push((format!("{}.skip.weight", $name), vec![$cout, $cin, 1, 1]));
                    out.push((format!("{}.skip.bias", $name), vec![$cout]));
                }
            }};
        }
        macro_rules! attn_block {
            ($name:expr, $c:expr) => {{
                for part in ["q", "k", "v", "o"] {
                    out.push((format!("{}.{part}.weight", $name), vec![$c, $c, 1, 1]));
                    out.push((format!("{}.{part}.bias", $name), vec![$c]));
                }
                norm!(format!("{}.n", $name), $c);
            }};
        }

        linear!("temb.mlp.0", self.time_dim * 4, self.time_dim);
        linear!("temb.mlp.2", self.time_dim, self.time_dim * 4);
        for (name, rows) in [
            ("cemb.sub", SUB_CLASSES.len() as u32 + 1),
            ("cemb.parent", PARENT_CLASSES.len() as u32 + 1),
            ("cemb.relief", RELIEF_CLASSES.len() as u32 + 1),
            ("cemb.texture", TEXTURE_CLASSES.len() as u32 + 1),
            ("cemb.water", WATER_CLASSES.len() as u32 + 1),
        ] {
            out.push((format!("{name}.weight"), vec![rows, cond_dim]));
        }
        linear!("cond_proj.0", cond_dim, cond_dim);
        linear!("cond_proj.2", cond_dim, cond_dim);
        norm!("cond_norm", cond_dim);
        conv!("input", c96, self.input_ch, 3);

        let channels = [c96, c192, c384, c768];
        for (i, &cout) in channels.iter().enumerate() {
            let cin = if i == 0 { cout } else { channels[i - 1] };
            let skip = cin != cout;
            res_block!(format!("downs.{i}.b1"), cin, cout, skip);
            res_block!(format!("downs.{i}.b2"), cout, cout, false);
            if i >= self.attn_from_level {
                attn_block!(format!("downs.{i}.attn"), cout);
            }
        }

        let last = c768;
        res_block!("mid.0.b1", last, last, false);
        attn_block!("mid.1.attn", last);
        res_block!("mid.2.b1", last, last, false);

        let up_cins = [c768 + c384, c384 + c192, c192 + c96, c96];
        for (i, &cin) in up_cins.iter().enumerate() {
            let cout = if i < 3 { channels[2 - i] } else { c96 };
            let skip = cin != cout;
            res_block!(format!("ups.{i}.b1"), cin, cout, skip);
            res_block!(format!("ups.{i}.b2"), cout, cout, false);
            if i == 0 {
                attn_block!(format!("ups.{i}.attn"), cout);
            }
        }

        norm!("out.0", c96);
        conv!("out.2", self.input_ch, c96, 3);
        out
    }

    /// Structural schema check for the conditional terrain UNet: every tensor
    /// named by [`Self::terrain_unet_layout`] must exist with the exact dims,
    /// so a wrong or partial pack is rejected before any GPU upload.
    pub fn validate_terrain_pack<'a>(&self, pack: &WeightPack<'a>) -> Result<(), AiError> {
        for (name, dims) in self.terrain_unet_layout() {
            pack.tensor(&name)?.expect_dims(&dims)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn small_pack() -> Vec<u8> {
        let meta = serde_json::to_vec(&json!({
            "model_kind": MODEL_KIND_TERRAIN_UNET_DDIM_V1,
            "dtype": "f32",
            "T": 1000,
            "base": 96,
            "schedule": "cosine",
            "source_ckpt": "test",
            "param_count": 4,
            "sha256": "0" .repeat(64),
            "created_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        let mut pack = Vec::new();
        pack.extend_from_slice(MAGIC);
        pack.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        pack.extend_from_slice(&0u32.to_le_bytes());
        pack.extend_from_slice(&DTYPE_F32.to_le_bytes());
        pack.extend_from_slice(&2u32.to_le_bytes());
        pack.extend_from_slice(&(meta.len() as u32).to_le_bytes());
        pack.extend_from_slice(&meta);
        for (name, dims, data) in [
            ("input.weight", vec![96u32, 1, 3, 3], vec![1.0f32; 96 * 9]),
            ("cemb.sub.weight", vec![24, 256], vec![5.0f32; 24 * 256]),
        ] {
            pack.extend_from_slice(&(name.len() as u32).to_le_bytes());
            pack.extend_from_slice(name.as_bytes());
            pack.extend_from_slice(&(dims.len() as u32).to_le_bytes());
            for d in dims {
                pack.extend_from_slice(&d.to_le_bytes());
            }
            let bytes: Vec<u8> = data.into_iter().flat_map(|v| v.to_le_bytes()).collect();
            pack.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            pack.extend_from_slice(&bytes);
        }
        pack
    }

    #[test]
    fn pack_parses_with_header_and_directory() {
        let bytes = small_pack();
        let pack = WeightPack::parse(&bytes).unwrap();
        assert_eq!(pack.model_kind, 0);
        assert_eq!(pack.meta.T, 1000);
        assert_eq!(pack.tensors.len(), 2);
        let conv = pack.tensor("input.weight").unwrap();
        assert_eq!(conv.dims, [96, 1, 3, 3]);
        assert_eq!(&conv.to_f32().unwrap()[..4], &[1.0, 1.0, 1.0, 1.0]);
        let emb = pack.tensor("cemb.sub.weight").unwrap();
        assert_eq!(emb.numel(), 24 * 256);
    }

    #[test]
    fn pack_rejects_bad_magic_and_truncation() {
        let mut bytes = small_pack();
        bytes[0] = b'X';
        assert!(WeightPack::parse(&bytes).is_err());
        let bytes = &small_pack()[..small_pack().len() - 3];
        assert!(WeightPack::parse(bytes).is_err());
    }

    #[test]
    fn cond_indices_map_names_and_null() {
        let cond = TerrainCond {
            sub: Some(0),
            parent: Some(5),
            relief: None,
            texture: Some(2),
            water: None,
        };
        assert_eq!(cond.indices(), [0, 5, 5, 2, 3]);
        assert!(cond.validate().is_ok());
        let bad = TerrainCond {
            sub: Some(23),
            ..cond
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn terrain_unet_layout_covers_executor_tensors() {
        let spec = TerrainUnetSpec::default_v1();
        let layout = spec.terrain_unet_layout();
        assert!(layout.iter().any(|(n, _)| n == "downs.0.b1.n1.weight"));
        assert!(layout.iter().any(|(n, _)| n == "downs.2.attn.n.bias"));
        assert!(layout.iter().any(|(n, _)| n == "ups.0.b1.film.weight"));
        assert!(layout.iter().any(|(n, _)| n == "cemb.water.weight"));
        assert!(layout.iter().any(|(n, _)| n == "out.2.bias"));
        // Skip convs exist exactly where cin != cout.
        assert!(layout.iter().any(|(n, _)| n == "downs.1.b1.skip.weight"));
        assert!(layout.iter().any(|(n, _)| n == "ups.0.b1.skip.weight"));
        assert!(!layout.iter().any(|(n, _)| n == "downs.0.b1.skip.weight"));
        assert!(!layout.iter().any(|(n, _)| n == "ups.3.b1.skip.weight"));
    }

    #[test]
    fn model_spec_validation_detects_malformed_packs() {
        let spec = TerrainUnetSpec::default_v1();
        assert_eq!(spec.channels(), [96, 192, 384, 768]);
        let result = spec.validate_terrain_pack(&WeightPack::parse(&small_pack()).unwrap());
        let error = result.unwrap_err();
        assert!(
            error.to_string().contains("temb.mlp.0.weight"),
            "the first required tensor must be reported by name: {error}"
        );
    }

    #[test]
    fn model_spec_validation_rejects_missing_residual_blocks() {
        let spec = TerrainUnetSpec::default_v1();
        let result = spec.validate_terrain_pack(&WeightPack::parse(&small_pack()).unwrap());
        assert!(result.is_err());
    }
}
