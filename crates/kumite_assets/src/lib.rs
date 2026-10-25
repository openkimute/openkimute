//! Loads the converted assets. The engine never touches the disc or original-hardware formats: everything here is
//! standard glTF 2.0 and JSON produced once by kumite_converter.
//!
//! - [`Character`]: skinned mesh, skeleton, every animation (`motion_NNN`, `common_NNN`) as 60 fps tracks.
//! - [`Stage`]: arena geometry flattened into world-space meshes, plus the camera-centred backdrop ("sky")
//!   and, for Force stages, the event nodes (glTF extras).
//!
//! Space: glTF metres, y up (the converters already applied the original hardware → glTF axis change).

pub mod avi;

use anyhow::{bail, Context, Result};
use glam::{Mat4, Quat, Vec3};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The converted data's layout this engine reads (manifest.json `format`).
pub const FORMAT: u64 = 8;

/// Asset root (`assets/` from convert_all).
#[derive(Clone, Debug)]
pub struct Assets {
    pub root: PathBuf,
}

impl Assets {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if !kumite_fs::exists(&root.join("manifest.json")) {
            bail!("{} holds no converted game (no manifest.json): convert your disc into it with kumite_converter", root.display());
        }
        let format = kumite_fs::read(&root.join("manifest.json")).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).and_then(|m| m["format"].as_u64()).unwrap_or(0);
        if format < FORMAT {
            bail!("{} holds an older conversion (format {format}, this OpenKumite reads {FORMAT}): convert your disc into it again with kumite_converter", root.display());
        }
        Ok(Self { root })
    }
    /// The fingerprint of what a fight reads (manifest.json's, from convert_all; empty when it has none).
    pub fn fingerprint(&self) -> String {
        let manifest = kumite_fs::read(&self.root.join("manifest.json")).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        manifest.and_then(|m| m["fingerprint"].as_str().map(str::to_string)).unwrap_or_default()
    }

    pub fn character(&self, model_record: usize) -> Result<Character> {
        Character::load(&self.root.join(format!("characters/model_{model_record:03}.glb")))
    }
    /// A character glb by file name (`characters/<file>`: the attract embu's own meshes).
    pub fn character_file(&self, file: &str) -> Result<Character> {
        // (a plain file name only: one with a folder or a root in it would read outside `characters/`)
        if std::path::Path::new(file).file_name() != Some(file.as_ref()) {
            bail!("a character file that is not a plain file name: {file:?}");
        }
        Character::load(&self.root.join("characters").join(file))
    }
    pub fn stage(&self, id: usize) -> Result<Stage> {
        Stage::load(&self.root.join(format!("stages/stage_{id:02}.glb")))
    }
    /// A JSON file under `gamedata/`, deserialised into `T`.
    pub fn gamedata<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<T> {
        let p = self.root.join("gamedata").join(name);
        let b = kumite_fs::read(&p).with_context(|| format!("opening {}", p.display()))?;
        Ok(serde_json::from_slice(&b)?)
    }
}

/// RGBA8 image (alpha 0 = transparent texel, the original hardware's colour-0 rule).
#[derive(Clone, Debug)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SkinVertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub joints: [u16; 4],
    pub weights: [f32; 4],
    /// The vertex normal (bind pose; zero when the model has none).
    pub normal: [f32; 3],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

/// A node of the skeleton with its rest transform.
#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub parent: Option<usize>,
    pub translation: Vec3,
    pub rotation: Quat,
}

/// One animation, sampled once per 60 Hz frame (the converter writes one key per game frame).
#[derive(Clone, Debug, Default)]
pub struct Clip {
    pub name: String,
    pub frames: usize,
    /// Per node: rotation per frame, or `None` if the node keeps its rest rotation.
    pub rotations: Vec<Option<Vec<Quat>>>,
    /// Per node: translation per frame (only the root carries one: root motion).
    pub translations: Vec<Option<Vec<Vec3>>>,
}

/// gamedata/shadows.json (export_shadows): the fighters' shadow meshes, each with the model records it serves.
#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct Shadows {
    pub meshes: Vec<ShadowMesh>,
}

/// A shadow mesh: vertices in their joints' local axes (metres), triangles and quads.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct ShadowMesh {
    pub models: Vec<usize>,
    pub joints: Vec<String>,
    /// (joint, x, y, z).
    pub vertices: Vec<(usize, f32, f32, f32)>,
    pub triangles: Vec<[u32; 3]>,
    pub quads: Vec<[u32; 4]>,
}

impl Shadows {
    /// The mesh a model record casts (the Force soldiers: none).
    pub fn of(&self, record: usize) -> Option<&ShadowMesh> {
        self.meshes.iter().find(|m| m.models.contains(&record))
    }
}

pub struct Character {
    pub name: String,
    pub nodes: Vec<Node>,
    /// Node order with parents first.
    order: Vec<usize>,
    /// Skin joints (node indices) and their inverse bind matrices.
    pub joints: Vec<usize>,
    pub inverse_bind: Vec<Mat4>,
    pub vertices: Vec<SkinVertex>,
    pub indices: Vec<u32>,
    pub texture: Image,
    pub clips: Vec<Clip>,
    clip_index: HashMap<String, usize>,
}

impl Character {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = kumite_fs::read(path).with_context(|| format!("loading {}", path.display()))?;
        Self::from_glb(&bytes).with_context(|| format!("loading {}", path.display()))
    }

    /// From a glb's bytes (any bytes: a bad file is an error, never a panic).
    pub fn from_glb(bytes: &[u8]) -> Result<Self> {
        unpanic(|| Self::parse_glb(bytes))
    }

    fn parse_glb(bytes: &[u8]) -> Result<Self> {
        let (doc, buffers, images) = import_glb(bytes)?;
        let get = |b: gltf::Buffer| Some(&buffers.get(b.index())?.0[..]);
        let nodes: Vec<Node> = doc
            .nodes()
            .map(|n| {
                let (t, r, _) = n.transform().decomposed();
                Node { name: n.name().unwrap_or_default().to_string(), parent: None, translation: t.into(), rotation: Quat::from_array(r) }
            })
            .collect();
        let mut nodes = nodes;
        for n in doc.nodes() {
            for c in n.children() {
                nodes[c.index()].parent = Some(n.index());
            }
        }
        let skin = doc.skins().next().context("no skin")?;
        let joints: Vec<usize> = skin.joints().map(|j| j.index()).collect();
        let inverse_bind: Vec<Mat4> = skin.reader(get).read_inverse_bind_matrices().context("no inverse bind matrices")?.map(|m| Mat4::from_cols_array_2d(&m)).collect();
        if inverse_bind.len() != joints.len() {
            bail!("a skin's inverse bind matrices are not one a joint");
        }
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mesh = doc.meshes().next().context("no mesh")?;
        for prim in mesh.primitives() {
            let r = prim.reader(get);
            let base = vertices.len() as u32;
            let pos: Vec<[f32; 3]> = r.read_positions().context("positions")?.collect();
            let uv: Vec<[f32; 2]> = r.read_tex_coords(0).context("uv")?.into_f32().collect();
            let jo: Vec<[u16; 4]> = r.read_joints(0).context("joints")?.into_u16().collect();
            let we: Vec<[f32; 4]> = r.read_weights(0).context("weights")?.into_f32().collect();
            let nm: Vec<[f32; 3]> = r.read_normals().map(|n| n.collect()).unwrap_or_default();
            if uv.len() < pos.len() || jo.len() < pos.len() || we.len() < pos.len() || jo.iter().flatten().any(|&j| j as usize >= joints.len()) {
                bail!("a primitive's attributes disagree");
            }
            for (i, (((&pos, &uv), &joints), &weights)) in pos.iter().zip(&uv).zip(&jo).zip(&we).enumerate() {
                vertices.push(SkinVertex { pos, uv, joints, weights, normal: nm.get(i).copied().unwrap_or_default() });
            }
            let prim_indices: Vec<u32> = r.read_indices().context("indices")?.into_u32().collect();
            if prim_indices.iter().any(|&i| i as usize >= pos.len()) {
                bail!("an index past the vertices");
            }
            indices.extend(prim_indices.into_iter().map(|i| i + base));
        }
        let texture = to_image(images.first().context("no texture")?)?;
        let mut clips = Vec::new();
        for a in doc.animations() {
            let mut clip = Clip { name: a.name().unwrap_or_default().to_string(), frames: 0, rotations: vec![None; nodes.len()], translations: vec![None; nodes.len()] };
            for ch in a.channels() {
                let r = ch.reader(get);
                let node = ch.target().node().index();
                match r.read_outputs().context("animation outputs")? {
                    gltf::animation::util::ReadOutputs::Rotations(q) => {
                        let v: Vec<Quat> = q.into_f32().map(|q| Quat::from_array(q).normalize()).collect();
                        clip.frames = clip.frames.max(v.len());
                        clip.rotations[node] = Some(v);
                    }
                    gltf::animation::util::ReadOutputs::Translations(t) => {
                        let v: Vec<Vec3> = t.map(Vec3::from).collect();
                        clip.frames = clip.frames.max(v.len());
                        clip.translations[node] = Some(v);
                    }
                    _ => {}
                }
            }
            clips.push(clip);
        }
        let clip_index = clips.iter().enumerate().map(|(i, c)| (c.name.clone(), i)).collect();
        let mut order = Vec::new();
        let mut placed = vec![false; nodes.len()];
        while order.len() < nodes.len() {
            let before = order.len();
            for i in 0..nodes.len() {
                if !placed[i] && nodes[i].parent.is_none_or(|p| placed[p]) {
                    placed[i] = true;
                    order.push(i);
                }
            }
            // (import_glb refuses cycles; this only keeps a loop impossible)
            if order.len() == before {
                bail!("the nodes make a cycle");
            }
        }
        let name = doc.scenes().next().and_then(|s| s.name().map(str::to_string)).unwrap_or_default();
        Ok(Self { name, nodes, order, joints, inverse_bind, vertices, indices, texture, clips, clip_index })
    }

    /// A shadow mesh skinned to this skeleton: each vertex in the bind pose, wholly on its joint; the quads split
    /// in two. None when a joint is missing (or the mesh's numbers point nowhere).
    pub fn shadow(&self, mesh: &ShadowMesh) -> Option<(Vec<SkinVertex>, Vec<u32>)> {
        let joints: Vec<usize> = mesh.joints.iter().map(|n| self.joints.iter().position(|&j| self.nodes.get(j).is_some_and(|node| node.name == *n))).collect::<Option<_>>()?;
        let vertices = mesh
            .vertices
            .iter()
            .map(|&(j, x, y, z)| {
                let joint = *joints.get(j)?;
                let pos = self.inverse_bind.get(joint)?.inverse().transform_point3(Vec3::new(x, y, z)).to_array();
                Some(SkinVertex { pos, joints: [joint as u16, 0, 0, 0], weights: [1.0, 0.0, 0.0, 0.0], ..Default::default() })
            })
            .collect::<Option<Vec<_>>>()?;
        let n = mesh.vertices.len() as u32;
        if mesh.triangles.iter().flatten().chain(mesh.quads.iter().flatten()).any(|&i| i >= n) {
            return None;
        }
        let mut indices: Vec<u32> = mesh.triangles.iter().flatten().copied().collect();
        indices.extend(mesh.quads.iter().flat_map(|q| [q[0], q[1], q[2], q[1], q[3], q[2]]));
        Some((vertices, indices))
    }

    pub fn clip(&self, name: &str) -> Option<usize> {
        self.clip_index.get(name).copied()
    }

    /// World matrix of every node for frame `frame` of clip `clip` (rest pose for nodes without a track).
    /// `lock_root_xz` removes horizontal root motion (the simulation moves the fighter itself).
    pub fn pose(&self, clip: usize, frame: usize, lock_root_xz: bool) -> Vec<Mat4> {
        let c = &self.clips[clip];
        let f = frame.min(c.frames.saturating_sub(1));
        let mut world = vec![Mat4::IDENTITY; self.nodes.len()];
        for &i in &self.order {
            let n = &self.nodes[i];
            let r = c.rotations[i].as_ref().map_or(n.rotation, |v| v[f.min(v.len() - 1)]);
            let mut t = c.translations[i].as_ref().map_or(n.translation, |v| v[f.min(v.len() - 1)]);
            if lock_root_xz && n.parent.is_none() && c.translations[i].is_some() {
                t.x = 0.0;
                t.z = 0.0;
            }
            let local = Mat4::from_rotation_translation(r, t);
            world[i] = n.parent.map_or(local, |p| world[p] * local);
        }
        world
    }

    /// Like [`Character::pose`], but playing another character's clip on this skeleton: tracks are matched by
    /// node name (the original's skeletons share their joint names); bones the other character lacks keep their
    /// rest pose, and only the root takes the other clip's translation (bone lengths stay this character's).
    pub fn pose_from(&self, other: &Character, clip: usize, frame: usize, lock_root_xz: bool) -> Vec<Mat4> {
        let c = &other.clips[clip];
        let f = frame.min(c.frames.saturating_sub(1));
        let mut world = vec![Mat4::IDENTITY; self.nodes.len()];
        for &i in &self.order {
            let n = &self.nodes[i];
            let j = other.nodes.iter().position(|o| o.name == n.name);
            let r = j.and_then(|j| c.rotations[j].as_ref()).map_or(n.rotation, |v| v[f.min(v.len() - 1)]);
            let mut t = n.translation;
            if n.parent.is_none() {
                if let Some(v) = j.and_then(|j| c.translations[j].as_ref()) {
                    t = v[f.min(v.len() - 1)];
                    if lock_root_xz {
                        t.x = 0.0;
                        t.z = 0.0;
                    }
                }
            }
            let local = Mat4::from_rotation_translation(r, t);
            world[i] = n.parent.map_or(local, |p| world[p] * local);
        }
        world
    }

    /// Skinning matrices (joint world × inverse bind) for a node pose, with a placement transform applied.
    pub fn skin_matrices(&self, node_world: &[Mat4], placement: Mat4) -> Vec<Mat4> {
        self.joints.iter().zip(&self.inverse_bind).map(|(&j, ibm)| placement * node_world[j] * *ibm).collect()
    }
}

/// A textured mesh in world space (texture = index into [`Stage::textures`]).
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub texture: usize,
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

/// A Force placement (glTF node extras written by export_stages).
#[derive(Clone, Debug)]
pub struct Placement {
    pub name: String,
    pub position: Vec3,
    pub extras: serde_json::Value,
}

pub struct Stage {
    pub name: String,
    pub textures: Vec<Image>,
    /// The backdrop drum (centred on the origin): the game draws it centred on the fighters' midpoint, its base
    /// `sky_base_y` metres below the floor, after the floor and without depth.
    pub sky: Vec<Mesh>,
    /// Depth of the drum's base ring below the floor (metres; the EXE's per-stage table 0x80025138).
    pub sky_base_y: f32,
    /// View-space Y scale of the drum against the world (its 5/3 Y row vs 7122/4096).
    pub sky_y_scale: f32,
    /// The drum is drawn before the floor (on the stage stages.json names), else after it.
    pub sky_first: bool,
    /// The full-screen fill behind everything (sRGB 0..255; 0x800482B0: one stage's sky blue, black elsewhere).
    pub background: [u8; 3],
    /// Repeat period of the floor map (m, x and z) when the floor is laid around the fighters (every arena but
    /// Force): the floor meshes may be shifted by whole periods.
    pub floor_period: Option<[f32; 2]>,
    /// Everything else, flattened to world space (floor copies, Force backdrop wall).
    pub meshes: Vec<Mesh>,
    /// Pickup sprite meshes (node-local; place at each item placement).
    pub item_mesh: Option<Mesh>,
    /// Ball's sky: a unit drum carrying the 2D strip, and where the strip goes (game units: its bottom at
    /// the point `bottom_ahead` in front of the camera at height `bottom_y`, y down; `height` px tall).
    pub strip: Option<(Mesh, [f32; 3])>,
    /// An extra stage: its sky is a unit drum centred on the camera (the panorama and its fills),
    /// drawn first without depth.
    pub camera_drum: bool,
    /// The extra stages' floor patch: tile size (m) and tiles a side, laid around the fighters' midpoint snapped to
    /// whole tiles (the engine clips the floor copies to it).
    pub floor_patch: Option<[f32; 2]>,
    /// The extra stages' floor lighting: mode, shade and the fade table (every 32nd entry), when the stage has it.
    pub floor_light: Option<(u32, f32, [f32; 32])>,
    pub placements: Vec<Placement>,
    /// Per texture (image): the glb asks for linear filtering (with mipmaps) and tiling both ways, as the
    /// extra levels do; the game's own stages ask for nearest and clamp.
    pub filtered: Vec<bool>,
    /// An extra level (scene extras `level`): its name.
    pub extra_level: Option<String>,
    /// Its walk grid (scene extras `walls`): origin (m, x / z), cell (m), size (cells), rows (bit set = blocked).
    pub walls: Option<StageWalls>,
    /// The free floor the camera may stand over (scene extras `camera_walls`; the fighters' grid when absent).
    pub camera_walls: Option<StageWalls>,
}

#[derive(Clone, Debug)]
pub struct StageWalls {
    pub origin: [f32; 2],
    pub cell: f32,
    pub size: [usize; 2],
    pub rows: Vec<Vec<u8>>,
}

impl Stage {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = kumite_fs::read(path).with_context(|| format!("loading {}", path.display()))?;
        Self::from_glb(&bytes).with_context(|| format!("loading {}", path.display()))
    }

    /// From a glb's bytes (any bytes: a bad file is an error, never a panic).
    pub fn from_glb(bytes: &[u8]) -> Result<Self> {
        unpanic(|| Self::parse_glb(bytes))
    }

    fn parse_glb(bytes: &[u8]) -> Result<Self> {
        let (doc, buffers, images) = import_glb(bytes)?;
        let get = |b: gltf::Buffer| Some(&buffers.get(b.index())?.0[..]);
        let textures = images.iter().map(to_image).collect::<Result<Vec<_>>>()?;
        let mesh_data = |m: gltf::Mesh, xf: Mat4| -> Result<Vec<Mesh>> {
            let mut out = Vec::new();
            for prim in m.primitives() {
                let r = prim.reader(get);
                let pos: Vec<[f32; 3]> = r.read_positions().context("positions")?.collect();
                let uv: Vec<[f32; 2]> = r.read_tex_coords(0).context("uv")?.into_f32().collect();
                let col: Vec<[f32; 4]> = match r.read_colors(0) {
                    Some(c) => c.into_rgba_f32().collect(),
                    None => vec![[1.0; 4]; pos.len()],
                };
                if uv.len() < pos.len() || col.len() < pos.len() {
                    bail!("a primitive's attributes disagree");
                }
                let texture = prim.material().pbr_metallic_roughness().base_color_texture().map_or(0, |t| t.texture().source().index());
                let vertices = pos.iter().zip(&uv).zip(&col).map(|((&p, &uv), &color)| Vertex { pos: xf.transform_point3(Vec3::from(p)).to_array(), uv, color }).collect();
                let indices: Vec<u32> = r.read_indices().context("indices")?.into_u32().collect();
                if indices.iter().any(|&i| i as usize >= pos.len()) {
                    bail!("an index past the vertices");
                }
                out.push(Mesh { texture, vertices, indices });
            }
            Ok(out)
        };
        let mut st = Stage {
            name: String::new(),
            textures,
            sky: vec![],
            sky_base_y: 0.0,
            sky_y_scale: 1.0,
            sky_first: false,
            background: [0; 3],
            floor_period: None,
            meshes: vec![],
            item_mesh: None,
            strip: None,
            camera_drum: false,
            floor_patch: None,
            floor_light: None,
            placements: vec![],
            filtered: vec![false; images.len()],
            extra_level: None,
            walls: None,
            camera_walls: None,
        };
        for t in doc.textures() {
            let s = t.sampler();
            if s.mag_filter() == Some(gltf::texture::MagFilter::Linear) {
                if let Some(f) = st.filtered.get_mut(t.source().index()) {
                    *f = true;
                }
            }
        }
        let scene = doc.scenes().next().context("no scene")?;
        st.name = scene.name().unwrap_or_default().to_string();
        if let Some(extras) = scene.extras() {
            let v: serde_json::Value = serde_json::from_str(extras.get())?;
            st.sky_base_y = v["drum_base_y"].as_f64().unwrap_or(0.0) as f32 / 1000.0;
            st.sky_y_scale = v["drum_y_scale"].as_f64().unwrap_or(1.0) as f32;
            st.sky_first = v["drum_before_floor"].as_bool().unwrap_or(false);
            if let Some(rgb) = v["background_rgb"].as_array() {
                st.background = std::array::from_fn(|k| rgb.get(k).and_then(|c| c.as_u64()).unwrap_or(0) as u8);
            }
            let p = &v["panorama_strip"];
            let strip_at = p.is_object().then(|| [p["bottom_ahead"].as_f64().unwrap_or(6656.0) as f32, p["bottom_y"].as_f64().unwrap_or(256.0) as f32, p["height"].as_f64().unwrap_or(256.0) as f32]);
            st.strip = strip_at.map(|a| (Mesh::default(), a));
            st.floor_period = v["floor_period"].as_array().and_then(|a| Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32]));
            st.camera_drum = v["camera_drum"].as_bool().unwrap_or(false);
            st.floor_patch = v["floor_patch"].as_array().and_then(|a| Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32]));
            st.extra_level = v["level"].as_str().map(str::to_string);
            let grid = |w: &serde_json::Value| {
                let f = |x: &serde_json::Value| x.as_f64().unwrap_or(0.0) as f32;
                let hex = |r: &str| (0..r.len() / 2).filter_map(|i| u8::from_str_radix(r.get(2 * i..2 * i + 2)?, 16).ok()).collect();
                w.is_object().then(|| StageWalls {
                    origin: [f(&w["origin"][0]), f(&w["origin"][1])],
                    cell: f(&w["cell"]),
                    size: [w["size"][0].as_u64().unwrap_or(0) as usize, w["size"][1].as_u64().unwrap_or(0) as usize],
                    rows: w["rows"].as_array().into_iter().flatten().map(|r| hex(r.as_str().unwrap_or_default())).collect(),
                })
            };
            st.walls = grid(&v["walls"]);
            st.camera_walls = grid(&v["camera_walls"]).or_else(|| st.walls.clone());
            let fl = &v["floor_light"];
            if fl.is_object() {
                let mut table = [0.0f32; 32];
                for (k, t) in fl["table"].as_array().into_iter().flatten().take(32).enumerate() {
                    table[k] = t.as_f64().unwrap_or(0.0) as f32;
                }
                st.floor_light = Some((fl["mode"].as_u64().unwrap_or(0) as u32, fl["shade"].as_f64().unwrap_or(0.0) as f32, table));
            }
        }
        let mut stack: Vec<(gltf::Node, Mat4)> = scene.nodes().map(|n| (n, Mat4::IDENTITY)).collect();
        while let Some((n, parent)) = stack.pop() {
            let xf = parent * Mat4::from_cols_array_2d(&n.transform().matrix());
            let name = n.name().unwrap_or_default();
            if let Some(extras) = n.extras() {
                let v: serde_json::Value = serde_json::from_str(extras.get())?;
                st.placements.push(Placement { name: name.to_string(), position: xf.w_axis.truncate(), extras: v });
                if let (Some(m), None) = (n.mesh(), &st.item_mesh) {
                    st.item_mesh = mesh_data(m, Mat4::IDENTITY)?.into_iter().next();
                }
            } else if let Some(m) = n.mesh() {
                if name == "sky" {
                    st.sky.extend(mesh_data(m, Mat4::IDENTITY)?);
                } else if name == "panorama_strip" {
                    if let (Some(mesh), Some(strip)) = (mesh_data(m, Mat4::IDENTITY)?.into_iter().next(), st.strip.as_mut()) {
                        strip.0 = mesh;
                    }
                } else {
                    st.meshes.extend(mesh_data(m, xf)?);
                }
            }
            stack.extend(n.children().map(|c| (c, xf)));
        }
        Ok(st)
    }
}

/// `gltf::import_slice`, but first the checks its validation lacks (gltf 1.4.1 panics, or asks for all the memory, on
/// such files: found by fuzzing, `fuzz/`, and review): a primitive's attribute naming no accessor, a buffer view
/// outside its buffer, a value out of range, an image not a PNG of the glb's own or too big, a sparse accessor or one
/// without data or outside its view (or strided tighter than its elements), an accessor of a kind its use cannot
/// take (gltf's readers have `unreachable!` there), an animation or skin naming no node, nodes not a tree. Only what
/// the converter writes passes.
fn import_glb(bytes: &[u8]) -> Result<(gltf::Document, Vec<gltf::buffer::Data>, Vec<gltf::image::Data>)> {
    let raw = gltf::Gltf::from_slice_without_validation(bytes)?;
    let json = raw.document.as_json();
    for m in &json.meshes {
        for p in &m.primitives {
            if p.attributes.values().any(|a| a.value() >= json.accessors.len()) {
                bail!("a mesh's attribute names no accessor");
            }
        }
    }
    for v in &json.buffer_views {
        let end = v.byte_offset.map_or(0, |o| o.0 as usize).checked_add(v.byte_length.0 as usize);
        let fits = json.buffers.get(v.buffer.value()).is_some_and(|b| end.is_some_and(|e| e <= b.byte_length.0 as usize));
        if !fits {
            bail!("a buffer view outside its buffer");
        }
    }
    // (a value the parser could not read, kept as `Invalid` and unwrapped by gltf later, does not serialise)
    if serde_json::to_writer(std::io::sink(), json).is_err() {
        bail!("a glb with values out of range");
    }
    // the images: PNGs in the glb's own data (as the converter writes them), of a size worth decoding
    let blob = raw.blob.as_deref().unwrap_or_default();
    let mut pixels = 0u64;
    for i in &json.images {
        let png = i.buffer_view.as_ref().and_then(|v| json.buffer_views.get(v.value())).filter(|v| v.buffer.value() == 0 && json.buffers.first().is_some_and(|b| b.uri.is_none()));
        let png = png.and_then(|v| {
            let at = v.byte_offset.map_or(0, |o| o.0 as usize);
            blob.get(at..at.checked_add(v.byte_length.0 as usize)?)
        });
        let size = png.filter(|_| i.mime_type.as_ref().is_some_and(|m| m.0 == "image/png")).and_then(png_size);
        if !size.is_some_and(|(w, h)| w <= MAX_TEXTURE && h <= MAX_TEXTURE) {
            bail!("an image that is not a PNG of the glb's own, or too big");
        }
        pixels += size.map_or(0, |(w, h)| u64::from(w) * u64::from(h));
    }
    if pixels > MAX_PIXELS {
        bail!("images of more pixels together than a glb has");
    }
    // (a character's clips keep a track per node)
    if json.animations.len().saturating_mul(json.nodes.len()) > MAX_ELEMENTS {
        bail!("more animations times nodes than a glb has");
    }
    // the accessors: plain (no sparse ones), with data, each element whole inside its view
    for a in &json.accessors {
        let (Some(view), true) = (a.buffer_view.as_ref().and_then(|v| json.buffer_views.get(v.value())), a.sparse.is_none()) else {
            bail!("an accessor without its own data");
        };
        let (gltf::json::validation::Checked::Valid(c), gltf::json::validation::Checked::Valid(t)) = (&a.component_type, &a.type_) else {
            bail!("an accessor of no type");
        };
        let element = c.0.size() * t.multiplicity();
        let stride = view.byte_stride.map_or(element, |s| s.0);
        let count = a.count.0 as usize;
        let end = (count.checked_sub(1)).and_then(|n| n.checked_mul(stride)).and_then(|n| n.checked_add(element)).and_then(|n| n.checked_add(a.byte_offset.map_or(0, |o| o.0 as usize)));
        if stride < element || count > MAX_ELEMENTS || !end.is_some_and(|e| e <= view.byte_length.0 as usize) {
            bail!("an accessor outside its view");
        }
    }
    // what reading it takes: a mesh as often as nodes place it, a sampler's output as often as channels use it (a small
    // file using one accessor again and again would otherwise ask for many times its size)
    let count = |i: usize| json.accessors.get(i).map_or(0, |a| a.count.0 as usize);
    let mut uses = vec![0usize; json.meshes.len()];
    for m in json.nodes.iter().filter_map(|n| n.mesh.as_ref()) {
        if let Some(u) = uses.get_mut(m.value()) {
            *u += 1;
        }
    }
    let meshes = json.meshes.iter().zip(&uses).map(|(m, &u)| {
        let size = m.primitives.iter().map(|p| p.attributes.values().map(|a| count(a.value())).sum::<usize>() + p.indices.as_ref().map_or(0, |i| count(i.value()))).sum::<usize>();
        size.saturating_mul(u.max(1))
    });
    let clips = json.animations.iter().flat_map(|a| a.channels.iter().filter_map(|c| a.samplers.get(c.sampler.value()))).map(|s| count(s.output.value()));
    if meshes.chain(clips).fold(0usize, usize::saturating_add) > MAX_READ {
        bail!("more to read than a glb has");
    }
    // each accessor of the kind gltf's readers take for its use (another is an `unreachable!` in them)
    {
        use gltf::json::accessor::{ComponentType as C, Type as T};
        use gltf::json::validation::Checked::Valid;
        let kind = |i: usize| match json.accessors.get(i) {
            Some(gltf::json::Accessor { component_type: Valid(c), type_: Valid(t), .. }) => Some((c.0, *t)),
            _ => None,
        };
        let is = |i: usize, ok: &dyn Fn(C, T) -> bool| kind(i).is_some_and(|(c, t)| ok(c, t));
        let float_or_norm = |c: C| matches!(c, C::F32 | C::U8 | C::U16);
        for p in json.meshes.iter().flat_map(|m| &m.primitives) {
            use gltf::json::mesh::Semantic as S;
            if p.targets.is_some() || !p.indices.as_ref().is_none_or(|i| is(i.value(), &|c, t| matches!(c, C::U8 | C::U16 | C::U32) && t == T::Scalar)) {
                bail!("a primitive's indices of no index type (or morph targets)");
            }
            for (semantic, a) in &p.attributes {
                let ok = match semantic {
                    Valid(S::Positions | S::Normals) => is(a.value(), &|c, t| c == C::F32 && t == T::Vec3),
                    Valid(S::Tangents) => is(a.value(), &|c, t| c == C::F32 && t == T::Vec4),
                    Valid(S::TexCoords(_)) => is(a.value(), &|c, t| float_or_norm(c) && t == T::Vec2),
                    Valid(S::Colors(_)) => is(a.value(), &|c, t| float_or_norm(c) && matches!(t, T::Vec3 | T::Vec4)),
                    Valid(S::Joints(_)) => is(a.value(), &|c, t| matches!(c, C::U8 | C::U16) && t == T::Vec4),
                    Valid(S::Weights(_)) => is(a.value(), &|c, t| float_or_norm(c) && t == T::Vec4),
                    _ => false,
                };
                if !ok {
                    bail!("a primitive's attribute of the wrong kind");
                }
            }
        }
        for an in &json.animations {
            for ch in &an.channels {
                use gltf::json::animation::Property as P;
                let Some(sampler) = an.samplers.get(ch.sampler.value()) else { bail!("an animation channel with no sampler") };
                let output = match ch.target.path {
                    Valid(P::Translation | P::Scale) => is(sampler.output.value(), &|c, t| c == C::F32 && t == T::Vec3),
                    Valid(P::Rotation) => is(sampler.output.value(), &|c, t| c != C::U32 && t == T::Vec4),
                    _ => false,
                };
                if ch.target.node.value() >= json.nodes.len() || !output || !is(sampler.input.value(), &|c, t| c == C::F32 && t == T::Scalar) {
                    bail!("an animation channel of the wrong kind, or of no node");
                }
            }
        }
        for sk in &json.skins {
            let ibm = sk.inverse_bind_matrices.as_ref().is_none_or(|i| is(i.value(), &|c, t| c == C::F32 && t == T::Mat4));
            if !ibm || sk.joints.iter().chain(&sk.skeleton).any(|j| j.value() >= json.nodes.len()) {
                bail!("a skin of the wrong kind, or of no node");
            }
        }
    }
    // the nodes: a tree (one parent at most, no cycle)
    let mut parent = vec![None; json.nodes.len()];
    for (i, n) in json.nodes.iter().enumerate() {
        for c in n.children.iter().flatten() {
            match parent.get_mut(c.value()) {
                Some(p @ None) => *p = Some(i),
                _ => bail!("a node with two parents"),
            }
        }
    }
    for i in 0..parent.len() {
        let (mut at, mut steps) = (i, 0);
        while let Some(p) = parent[at] {
            (at, steps) = (p, steps + 1);
            if steps > parent.len() {
                bail!("the nodes make a cycle");
            }
        }
    }
    Ok(gltf::import_slice(bytes)?)
}

/// A glb's largest texture side (pixels), its images' pixels together (the largest converted ~2.1M), its largest
/// accessor (elements) and animations times nodes.
const MAX_TEXTURE: u32 = 4096;
const MAX_PIXELS: u64 = 1 << 24;
const MAX_ELEMENTS: usize = 1 << 22;
/// The elements a glb's meshes and clips read at most, all told (the converter's largest: about 2.4 million).
const MAX_READ: usize = 2 * MAX_ELEMENTS;

/// A PNG's width and height (its header's IHDR).
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.get(..8)? != b"\x89PNG\r\n\x1a\n" || b.get(12..16)? != b"IHDR" {
        return None;
    }
    let be = |o: usize| Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?));
    Some((be(16)?, be(20)?))
}

/// Natively a panic still left in gltf on a bad file is an error; on the web (no unwinding) the checks of
/// [`import_glb`] are what keep it from one.
fn unpanic<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| bail!("a bad glb"))
}

fn to_image(d: &gltf::image::Data) -> Result<Image> {
    use gltf::image::Format;
    let rgba = match d.format {
        Format::R8G8B8A8 => d.pixels.clone(),
        Format::R8G8B8 => d.pixels.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        f => bail!("unsupported texture format {f:?}"),
    };
    Ok(Image { width: d.width, height: d.height, rgba })
}
