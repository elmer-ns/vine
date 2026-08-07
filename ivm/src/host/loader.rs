use ivy::{
  name::Table,
  text::{ast::Diag, parser::Parser},
};

use crate::{host::Host, program::Program, runtime::graft::Graft};

pub struct IvyLoader<'ivm> {
  table: Table,
  programs: Vec<Program<'ivm>>,
}

#[derive(Debug)]
pub enum LoadError {
  Ivy(Diag),
  MissingMain,
}

impl From<Diag> for LoadError {
  fn from(error: Diag) -> Self {
    LoadError::Ivy(error)
  }
}

impl<'ivm> IvyLoader<'ivm> {
  /// Creates a loader from the name table used to register the host's
  /// extrinsics. All names extrinsics must be registered before this call
  pub fn new(table: &Table) -> Self {
    Self { table: table.clone(), programs: Vec::new() }
  }

  /// # Panics
  ///
  /// May panic if the parsed Ivy cannot be encoded by the IVM, including
  /// references to unregistered extrinsics or malformed node arities.
  pub fn load(&mut self, host: &Host<'ivm>, src: &str) -> Result<&'ivm Graft<'ivm>, LoadError> {
    let nets = Parser::parse(&mut self.table, src)?;
    let nets = nets.to_flat_nets()?;

    let main = self.table.add_path_name("iv:main");

    if !nets.contains_key(&main) {
      return Err(LoadError::MissingMain);
    }

    let program = Program::new(host, &mut self.table, &nets);

    self.programs.push(program);

    Ok(self.programs.last().unwrap().graft(main).expect("main was checked before building"))
  }
}

#[cfg(test)]
mod tests {
  use std::ptr::eq;

  use crate::{
    host::{IVM, ext::common::IO, runner::CaptureOutput},
    runtime::{
      heap::Heap,
      port::{Port, Tag},
    },
  };

  use super::*;

  const IDENTITY_MAIN: &str = r#"
          iv:main {
              ^ = ivm:x(io io)
          }
      "#;

  #[test]
  fn loads_and_runs_main() {
    let mut heap = Heap::new();

    let mut ivm = IVM::new();
    let mut host = Host::new(&mut ivm);
    let table = Table::default();

    let io = host.register_ext_ty::<IO>();

    let mut loader = IvyLoader::new(&table);
    let main = loader.load(&host, IDENTITY_MAIN).unwrap();

    let mut runtime = host.init(&mut heap);

    // This is the same entry-point arrangement used by Runner.
    let root = unsafe { runtime.new_node(Tag::Comb, 0) };

    runtime.link_wire(root.1, Port::new_ext_val(io.wrap_static(IO)));
    runtime.link(Port::new_graft(main), root.0);

    runtime.normalize(());

    let result = runtime.follow(Port::new_wire(root.2));

    assert_eq!(result.tag(), Tag::ExtVal);
    assert_eq!(unsafe { result.as_ext_val() }.ty_id(), io.id(),);
  }

  const MISSING_MAIN: &str = r#"
          iv:not_main {
              ^ = ivm:x(io io)
          }
      "#;

  #[test]
  fn reports_missing_main() {
    let mut ivm = IVM::new();
    let host = Host::new(&mut ivm);
    let table = Table::default();

    let mut loader = IvyLoader::new(&table);

    assert!(matches!(loader.load(&host, MISSING_MAIN), Err(LoadError::MissingMain)));
  }

  const INVALID_IVY: &str = r#"
    this ain't how you write ivy
    def fn main() {
        println!("Hello, world!");
    }
      "#;

  #[test]
  fn reports_invalid_ivy() {
    let mut ivm = IVM::new();
    let host = Host::new(&mut ivm);
    let table = Table::default();

    let mut loader = IvyLoader::new(&table);

    assert!(matches!(loader.load(&host, INVALID_IVY), Err(LoadError::Ivy(_))));
  }

  #[test]
  fn can_load_multiple_programs() {
    let mut ivm = IVM::new();
    let host = Host::new(&mut ivm);
    let table = Table::default();

    let mut loader = IvyLoader::new(&table);

    let first_main = loader.load(&host, IDENTITY_MAIN).unwrap();
    let second_main = loader.load(&host, IDENTITY_MAIN).unwrap();

    assert!(!eq(first_main, second_main));
  }

  const HI: &str = include_str!("../../examples/hi.iv");

  #[test]
  fn loads_and_runs_hi() {
    let capture = CaptureOutput::default();
    let args = Vec::<String>::new();
    let mut heap = Heap::new();
    let mut ivm = IVM::new();

    {
      let mut host = Host::new(&mut ivm);
      let mut table = Table::default();

      let io = host.register_ext_ty::<IO>();

      // Registers arithmetic, I/O, and other standard extrinsics.
      host.register(&mut table, capture.extrinsics(&args));

      // Clone the table only after registering the named extrinsics.
      let mut loader = IvyLoader::new(&table);
      let main = loader.load(&host, HI).unwrap();

      let mut runtime = host.init(&mut heap);
      let root = unsafe { runtime.new_node(Tag::Comb, 0) };

      runtime.link_wire(root.1, Port::new_ext_val(io.wrap_static(IO)));
      runtime.link(Port::new_graft(main), root.0);

      runtime.normalize(());

      // Confirm execution returned the IO capability properly.
      let result = runtime.follow(Port::new_wire(root.2));
      assert_eq!(result.tag(), Tag::ExtVal);
      assert_eq!(unsafe { result.as_ext_val() }.ty_id(), io.id(),);
    }

    // Host must be dropped before consuming CaptureOutput.
    let output = capture.into_output();

    assert_eq!(String::from_utf8(output).unwrap(), "Hi\n");
  }
}
