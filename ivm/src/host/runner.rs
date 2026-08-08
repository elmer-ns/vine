use std::{
  collections::HashMap,
  io::{self, Write},
  sync::{Mutex, MutexGuard},
};

use ivy::{
  name::{NameId, Table},
  net::FlatNet,
};
use vine_util::register::Register;

use crate::{
  host::{
    Host,
    dynamic::{DynamicProgramRequest, DynamicProgramService},
    ext::common::{self, IO},
    loader::DynamicProgramLoader,
    module::{CompileError, GraftHandle},
  },
  program::Program,
  runtime::{
    Hooks, Runtime,
    ext::ExtTy,
    flags::Flags,
    heap::Heap,
    port::{Port, Tag},
    stats::Stats,
    wire::Wire,
  },
};

pub struct Runner<'ivm, 'ext> {
  host: &'ext Host<'ivm>,
  ivy_loader: DynamicProgramLoader,
  ivy_service: DynamicProgramService<'ivm>,
  ivy_errors: Vec<CompileError>,

  io: ExtTy<'ivm, IO>,
  runtime: Runtime<'ivm, 'ext>,
  root: Wire<'ivm>,
}

impl<'ivm, 'ext> Runner<'ivm, 'ext> {
  pub fn new(
    heap: &'ivm mut Heap,
    host: &'ext mut Host<'ivm>,
    extrinsics: impl Register<Host<'ivm>>,
    table: &mut Table,
    nets: &HashMap<NameId, FlatNet>,
  ) -> Self {
    let io = host.register_ext_ty::<IO>();

    let ivy_requests = DynamicProgramService::new(host, table);

    host.register(table, extrinsics);

    let ivy_loader = DynamicProgramLoader::new(table);

    let program = Program::new(host, table, nets);
    let main = table.add_path_name("iv:main");
    let main = program.graft(main).expect("missing main");

    let host: &'ext Host<'ivm> = host;

    let mut runtime = host.init(heap);

    let node = unsafe { runtime.new_node(Tag::Comb, 0) };
    runtime.link_wire(node.1, Port::new_ext_val(io.wrap_static(IO)));
    runtime.link(Port::new_graft(main), node.0);

    Self {
      host,
      ivy_loader,
      ivy_service: ivy_requests,
      ivy_errors: Vec::new(),
      io,
      root: node.2,
      runtime,
    }
  }

  pub fn normalize(
    mut self,
    breadth_first: bool,
    workers: usize,
    mut hooks: impl Hooks,
  ) -> RunOutcome {
    loop {
      // One normalization epoch.
      if breadth_first {
        self.runtime.normalize_breadth_first(&mut hooks);
      } else if workers > 0 {
        self.runtime.normalize_parallel(workers)
      } else {
        self.runtime.normalize(&mut hooks);
      }

      // Splicing requests creates new active work, so run another epoch.
      if !self.service_ivy_requests() {
        break;
      }
    }

    // Only inspect the result after all dynamically loaded nets have run.
    let out = self.runtime.follow(Port::new_wire(self.root));

    self.runtime.flags.no_io =
      out.tag() != Tag::ExtVal || unsafe { out.as_ext_val() }.ty_id() != self.io.id();
    self.runtime.flags.vicious = self.runtime.stats.mem_free < self.runtime.stats.mem_alloc;

    RunOutcome { stats: self.runtime.stats, flags: self.runtime.flags, ivy_errors: self.ivy_errors }
  }

  fn service_ivy_requests(&mut self) -> bool {
    let requests = self.ivy_service.drain();

    if requests.is_empty() {
      return false;
    }

    for request in requests {
      match request {
        DynamicProgramRequest::ParseIvy { source, output } => {
          let result =
            self.ivy_loader.parse_ivy(self.host, &source).map_err(|error| error.to_string());

          self.ivy_service.write_compile_result(&mut self.runtime, output, result);
        }
        DynamicProgramRequest::Resolve { module, entry, output } => {
          let result = self
            .ivy_loader
            .graft(module, &entry)
            .map(GraftHandle::new)
            .ok_or_else(|| format!("missing Ivy entry '{entry}'"));

          self.ivy_service.write_resolve_result(&mut self.runtime, output, result);
        }
      }
    }

    true
  }
}

/*IvyRequest::Run { module, entry, input, result_output } => {
  match self.ivy_loader.entry(module, &entry) {
    Some(entry) => {
      let entry_node = unsafe { self.runtime.new_node(Tag::Comb, 0) };

      let finish_node =
        unsafe { self.runtime.new_node(Tag::ExtFn, self.ivy_service.finish_ok_label()) };

      // I → loaded iv:main
      self.runtime.link_wire(entry_node.1, Port::new_ext_val(input));

      // Loaded O → finish_ok principal port
      self.runtime.link_wire(entry_node.2, finish_node.0);

      // finish_ok's first output → caller's Result
      self.runtime.link_wire_wire(finish_node.1, result_output);

      // The one-output finish extrinsic does not use its second auxiliary port.
      self.runtime.link_wire(finish_node.2, Port::ERASE);

      // Start the loaded graft.
      self.runtime.link(Port::new_graft(entry), entry_node.0);
    }
    None => {
      self.ivy_service.write_run_error(
        &mut self.runtime,
        result_output,
        String::from(format!("Entry '{}' doesn't exist", entry)),
        input,
      );
    }
  }
}*/

#[derive(Default)]
pub struct CaptureOutput {
  pub output: Mutex<Vec<u8>>,
}

impl CaptureOutput {
  pub fn extrinsics<'a: 'ivm, 'b: 'ivm, 'ivm>(
    &'a self,
    args: &'b [String],
  ) -> impl Register<Host<'ivm>> where {
    common::all(args, || &[][..], || SharedWriter(self.output.lock().unwrap()))
  }

  pub fn into_output(self) -> Vec<u8> {
    self.output.into_inner().unwrap()
  }
}

struct SharedWriter<'a>(MutexGuard<'a, Vec<u8>>);

impl Write for SharedWriter<'_> {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.0.extend_from_slice(buf);
    Ok(buf.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
  }
}

pub struct RunOutcome {
  pub stats: Stats,
  pub flags: Flags,
  pub ivy_errors: Vec<CompileError>,
}

impl RunOutcome {
  pub fn success(&self) -> bool {
    self.flags.success() && self.ivy_errors.is_empty()
  }

  pub fn error_message(&self, debug_hint: bool) -> String {
    let mut errors = self
      .ivy_errors
      .iter()
      .map(|error| format!("Error: dynamic Ivy load failed: {error}"))
      .collect::<Vec<_>>();

    let runtime_errors = self.flags.error_message(debug_hint);

    if !runtime_errors.is_empty() {
      errors.push(runtime_errors);
    }

    errors.join("\n\n")
  }
}
