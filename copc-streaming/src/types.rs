/// Octree node key: (level, x, y, z).
///
/// All fields are `i32` to match the COPC wire format. `level` is always
/// non-negative in practice; `x`, `y`, `z` are signed per the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoxelKey {
    /// Octree depth (0 = root).
    pub level: i32,
    /// X coordinate at this level.
    pub x: i32,
    /// Y coordinate at this level.
    pub y: i32,
    /// Z coordinate at this level.
    pub z: i32,
}

impl VoxelKey {
    /// The root octree node.
    pub const ROOT: VoxelKey = VoxelKey {
        level: 0,
        x: 0,
        y: 0,
        z: 0,
    };

    /// Return the parent key, or `None` for the root node.
    pub fn parent(&self) -> Option<VoxelKey> {
        if self.level == 0 {
            return None;
        }
        Some(VoxelKey {
            level: self.level - 1,
            x: self.x >> 1,
            y: self.y >> 1,
            z: self.z >> 1,
        })
    }

    /// Return the child key in the given octant direction (0–7).
    pub fn child(&self, dir: u8) -> VoxelKey {
        debug_assert!(dir < 8, "octant direction must be 0–7");
        VoxelKey {
            level: self.level + 1,
            x: (self.x << 1) | i32::from(dir & 0x1),
            y: (self.y << 1) | i32::from((dir >> 1) & 0x1),
            z: (self.z << 1) | i32::from((dir >> 2) & 0x1),
        }
    }

    /// Return all eight child keys.
    pub fn children(&self) -> [VoxelKey; 8] {
        std::array::from_fn(|i| self.child(i as u8))
    }

    /// Compute the spatial bounding box of this node given the root octree bounds.
    pub fn bounds(&self, root_bounds: &Aabb) -> Aabb {
        let side = (root_bounds.max[0] - root_bounds.min[0]) / 2_u32.pow(self.level as u32) as f64;
        Aabb {
            min: [
                root_bounds.min[0] + self.x as f64 * side,
                root_bounds.min[1] + self.y as f64 * side,
                root_bounds.min[2] + self.z as f64 * side,
            ],
            max: [
                root_bounds.min[0] + (self.x + 1) as f64 * side,
                root_bounds.min[1] + (self.y + 1) as f64 * side,
                root_bounds.min[2] + (self.z + 1) as f64 * side,
            ],
        }
    }
}

/// Axis-aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    /// Minimum corner `[x, y, z]`.
    pub min: [f64; 3],
    /// Maximum corner `[x, y, z]`.
    pub max: [f64; 3],
}

impl Aabb {
    /// Test whether two bounding boxes overlap.
    pub fn intersects(&self, other: &Aabb) -> bool {
        self.min[0] <= other.max[0]
            && self.max[0] >= other.min[0]
            && self.min[1] <= other.max[1]
            && self.max[1] >= other.min[1]
            && self.min[2] <= other.max[2]
            && self.max[2] >= other.min[2]
    }
}
