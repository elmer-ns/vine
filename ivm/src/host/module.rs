use ivy::name::NameId;

use crate::{
  host::ext::ExtTyRegister,
  program::Program,
  runtime::{ext::ExtTyCastStatic, graft::Graft, word::Word},
};

pub enum RunError {
  EntryDoesntExist,
}

#[derive(Debug)]
pub enum CompileError {}

/// An immutable, reusable handle to compiled Ivy code.
///
/// The referenced graft is owned by the IVM's graft arena and remains valid
/// for the entire `'ivm` lifetime.
#[derive(Clone, Copy)]
pub struct IvyModule<'ivm> {
  program: &'ivm Program<'ivm>,
}

// SAFETY:
//
// - Every graft pointer targets an allocation owned by the IVM graft arena.
// - Those allocations remain stable for the entire `'ivm` lifetime.
// - Grafts are fully initialized before the Program is shared.
// - Resolving an entry only reads the map and referenced graft.
// - Mutating the map requires exclusive `&mut Program` access.
unsafe impl<'ivm> Send for Program<'ivm> {}
unsafe impl<'ivm> Sync for Program<'ivm> {}

impl<'ivm> IvyModule<'ivm> {
  pub fn new(program: &'ivm Program<'ivm>) -> Self {
    Self { program }
  }

  pub fn graft(&self, name: NameId) -> Option<&'ivm Graft<'ivm>> {
    self.program.graft(name)
  }
}

impl<'ivm> ExtTyRegister<'ivm> for IvyModule<'ivm> {
  type With<'x> = IvyModule<'x>;
}

impl<'ivm> ExtTyCastStatic<'ivm> for IvyModule<'ivm> {
  const COPY: bool = true;

  fn into_payload_static(module: Self) -> Word {
    Word::from_ptr(module.program as *const Program<'ivm> as *const ())
  }

  unsafe fn from_payload_static(payload: Word) -> Self {
    Self { program: unsafe { &*(payload.ptr() as *const Program<'ivm>) } }
  }
}

#[cfg(test)]
pub mod tests {
  use ivy::name::Table;

  use crate::host::{Host, IVM, loader::IvyLoader};

  use super::*;

  const MULTI_ENTRY: &str = r#"
    iv:first {
      ^ = ivm:x(value value)
    }

    iv:second {
      ^ = ivm:x(value value)
    }
    "#;

  #[test]
  fn compiles_all_entry_points() {
    let mut ivm = IVM::new();
    let host = Host::new(&mut ivm);
    let table = Table::default();
    let mut loader = IvyLoader::new(&table);

    let module = loader.compile(&host, MULTI_ENTRY).unwrap();

    assert!(loader.entry(module, "iv:first").is_some());
    assert!(loader.entry(module, "iv:second").is_some());
    assert!(loader.entry(module, "iv:missing").is_none());
  }
}
