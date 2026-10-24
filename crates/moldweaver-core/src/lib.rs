//! MoldWeaver core: everything except the user interface.
//!
//! * [`mesh`] – triangle meshes, topology checks and repair
//! * [`io`] – STL/OBJ/3MF import and 3MF export
//! * [`frame`] – the parting plane and the mold coordinate frame
//! * [`analysis`] – undercut detection and plane suggestions
//! * [`molds`] – the [`molds::MoldType`] trait and the mold generators
//! * [`layout`] – print orientation and plate packing
//! * [`printer`] – printer profiles
//! * [`project`] – versioned project files
//! * [`pipeline`] – glue used by the GUI and the CLI

pub mod analysis;
pub mod frame;
pub mod io;
pub mod layout;
pub mod mesh;
pub mod molds;
pub mod pipeline;
pub mod printer;
pub mod project;
pub mod solid;

pub use glam;
pub use manifold_rust;
