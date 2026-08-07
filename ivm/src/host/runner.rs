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
    dynamic::{IvyRequest, IvyService},
    ext::common::{self, IO, Nil},
    loader::{IvyLoader, LoadError},
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
  ivy_loader: IvyLoader<'ivm>,
  ivy_service: IvyService<'ivm>,
  ivy_errors: Vec<LoadError>,

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

    let ivy_requests = IvyService::new(host, table);

    host.register(table, extrinsics);

    let ivy_loader = IvyLoader::new(table);

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
      let IvyRequest { source, io, result_output, io_output } = request;

      match self.ivy_loader.load(self.host, &source) {
        Ok(main) => {
          // `Ok(())`: source loaded and main was spliced successfully.
          self.ivy_service.write_results(&mut self.runtime, result_output, Ok(Nil));

          // Adapt the loaded main's IO→IO boundary to the suspended
          // extrinsic input and output.
          let node = unsafe { self.runtime.new_node(Tag::Comb, 0) };

          self.runtime.link_wire(node.1, Port::new_ext_val(io));

          self.runtime.link_wire_wire(node.2, io_output);

          self.runtime.link(Port::new_graft(main), node.0);
        }
        Err(error) => {
          self.ivy_service.write_results(&mut self.runtime, result_output, Err(error.to_string()));

          // Preserve the IO continuation despite the load failure.
          self.runtime.link_wire(io_output, Port::new_ext_val(io));
        }
      }
    }

    true
  }
}

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
  pub ivy_errors: Vec<LoadError>,
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

#[cfg(test)]
mod tests {
  use super::*;

  use crate::host::{
    IVM,
    dynamic::{IvyRequest, IvyService},
    ext::common::Pair,
  };

  const IDENTITY_MAIN: &str = r#"
    iv:main {
      ^ = ivm:x(io io)
    }
    "#;

  #[test]
  fn services_ivy_request_during_normalization() {
    let mut heap = Heap::new();
    let mut ivm = IVM::new();
    let mut host = Host::new(&mut ivm);
    let mut table = Table::default();

    let io = host.register_ext_ty::<IO>();

    let ivy_requests = IvyService::new(&mut host, &mut table);

    // Clone the table after registering root:ivy:run.
    let ivy_loader = IvyLoader::new(&table);

    // Host is immutable throughout execution.
    let host = &host;
    let mut runtime = host.init(&mut heap);

    let (result_output, result_root) = runtime.new_wire();
    let (io_output, root) = runtime.new_wire();

    ivy_requests.push(IvyRequest {
      source: IDENTITY_MAIN.to_owned(),
      io: io.wrap_static(IO),
      result_output,
      io_output,
    });

    // We will inspect the IO result manually, so make one unused Runner-root
    // copy before moving `root` into Runner. Runner::normalize is not called.
    let root_for_assert = unsafe { root.clone() };

    // We construct Runner directly because this test starts at the precise
    // state immediately after root:ivy:run has enqueued its request.
    let mut runner = Runner {
      host,
      ivy_loader,
      ivy_service: ivy_requests,
      ivy_errors: Vec::new(),
      io,
      runtime,
      root,
    };

    assert!(runner.service_ivy_requests());

    // This executes the graft spliced by the service.
    runner.runtime.normalize(());

    // Consume and inspect the Result[(), String] output.
    let result = runner.runtime.follow(Port::new_wire(result_root));

    assert_eq!(result.tag(), Tag::ExtVal);

    let pair = host.get_ext_ty::<Pair>().unwrap();

    let Pair(tag, value) = pair
      .unwrap(&mut runner.runtime, unsafe { result.as_ext_val() })
      .expect("Ivy::run should return an encoded result");

    let n32 = host.get_ext_ty::<u32>().unwrap();

    assert_eq!(
      n32.unwrap_static(tag),
      Some(1), // 1 = Ok
    );

    let nil = host.get_ext_ty::<Nil>().unwrap();

    assert!(nil.unwrap_static(value).is_some(), "Ok result should contain Vine () / IVM Nil",);

    // Consume and inspect the returned IO continuation.
    let output = runner.runtime.follow(Port::new_wire(root_for_assert));

    assert_eq!(output.tag(), Tag::ExtVal);

    assert_eq!(unsafe { output.as_ext_val() }.ty_id(), io.id(),);

    assert!(runner.ivy_errors.is_empty());
    assert!(runner.runtime.flags.success());

    assert_eq!(
      runner.runtime.stats.mem_free, runner.runtime.stats.mem_alloc,
      "the test must consume both Result and IO continuations",
    );
  }
}
