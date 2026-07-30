//! The field map: every string a secret could hide in, named by pointer.

use crate::detect::FieldShape;

/// One field the map named, whether or not anything was found in it. A
/// surface with zero findings is information: the pass reached that field
/// and the detectors were silent.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Surface {
    pub step: String,
    pub at: String,
    pub shape: FieldShape,
    pub bytes: usize,
}

pub fn surfaces(_path: &toolpath::v1::Path) -> Vec<Surface> {
    todo!("T2")
}

/// Resolves a `(step, pointer)` pair against a document for reading and
/// writing. Read and write must resolve identically.
pub struct SurfaceCursor<'a> {
    pub path: &'a mut toolpath::v1::Path,
}

impl SurfaceCursor<'_> {
    pub fn read(&self, _step: &str, _at: &str) -> Option<String> {
        todo!("T2")
    }

    pub fn write(&mut self, _step: &str, _at: &str, _value: &str) -> crate::Result<()> {
        todo!("T2")
    }
}

pub fn ptr_escape(_token: &str) -> String {
    todo!("T2")
}
