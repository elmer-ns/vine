use ivy::{name::Table, text::parser::Parser};

use crate::{
  host::{
    Host,
    module::{CompileError, ProgramHandle},
  },
  program::Program,
  runtime::graft::Graft,
};

pub struct DynamicProgramLoader {
  table: Table,
}

impl<'ivm> DynamicProgramLoader {
  /// Creates a loader from the name table used to register the host's
  /// extrinsics. All names extrinsics must be registered before this call
  pub fn new(table: &Table) -> Self {
    Self { table: table.clone() }
  }

  /// # Panics
  ///
  /// May panic if the parsed Ivy cannot be encoded by the IVM, including
  /// references to unregistered extrinsics or malformed node arities.
  pub fn parse_ivy(
    &mut self,
    host: &Host<'ivm>,
    src: &str,
  ) -> Result<ProgramHandle<'ivm>, CompileError> {
    let nets = Parser::parse(&mut self.table, src)?;
    let nets = nets.to_flat_nets()?;

    let program = Program::new(host, &mut self.table, &nets);

    Ok(ProgramHandle::new(host.ivm.programs.push(Box::new(program))))
  }

  pub fn graft(&mut self, module: ProgramHandle<'ivm>, path: &str) -> Option<&'ivm Graft<'ivm>> {
    let name = self.table.add_path_name(path);
    module.graft(name)
  }
}
