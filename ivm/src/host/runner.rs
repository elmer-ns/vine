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
    dynamic::{IvyRequest, IvyRequests},
    ext::common::{self, IO},
    loader::IvyLoader,
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
  ivy_requests: IvyRequests<'ivm>,

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

    let ivy_requests = IvyRequests::default();

    host.register(table, (extrinsics, ivy_requests.extrinsics()));

    let ivy_loader = IvyLoader::new(table);

    let program = Program::new(host, table, nets);
    let main = table.add_path_name("iv:main");
    let main = program.graft(main).expect("missing main");

    let host: &'ext Host<'ivm> = host;

    let mut runtime = host.init(heap);

    let node = unsafe { runtime.new_node(Tag::Comb, 0) };
    runtime.link_wire(node.1, Port::new_ext_val(io.wrap_static(IO)));
    runtime.link(Port::new_graft(main), node.0);

    Self { host, ivy_loader, ivy_requests, io, root: node.2, runtime }
  }

  pub fn normalize(
    mut self,
    breadth_first: bool,
    workers: usize,
    mut hooks: impl Hooks,
  ) -> (Stats, Flags) {
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

    (self.runtime.stats, self.runtime.flags)
  }

  fn service_ivy_requests(&mut self) -> bool {
    let requests = self.ivy_requests.drain();

    if requests.is_empty() {
      return false;
    }

    for request in requests {
      let IvyRequest { source, io, output } = request;

      match self.ivy_loader.load(self.host, &source) {
        Ok(main) => {
          // Adapt the loaded main's IO→IO boundary to the suspended
          // extrinsic input and output.
          let node = unsafe { self.runtime.new_node(Tag::Comb, 0) };

          self.runtime.link_wire(node.1, Port::new_ext_val(io));

          self.runtime.link_wire_wire(node.2, output);

          self.runtime.link(Port::new_graft(main), node.0);
        }
        Err(_error) => {
          // Preserve the IO chain so the graph remains structurally
          // usable, while recording that execution failed.
          self.runtime.flags.ext_generic = true;
          self.runtime.link_wire(output, Port::new_ext_val(io));
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

#[cfg(test)]
mod tests {
  use super::*;

  use crate::host::{
    IVM,
    dynamic::{IvyRequest, IvyRequests},
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

    let ivy_requests = IvyRequests::default();
    host.register(&mut table, ivy_requests.extrinsics());

    // Clone the table after registering root:ivy:run.
    let ivy_loader = IvyLoader::new(&table);

    // Host is immutable throughout execution.
    let host = &host;
    let mut runtime = host.init(&mut heap);

    // `output` is handed to the pending request.
    // `root` is where Runner expects the resulting IO.
    let (output, root) = runtime.new_wire();

    ivy_requests.push(IvyRequest {
      source: IDENTITY_MAIN.to_owned(),
      io: io.wrap_static(IO),
      output,
    });

    // We construct Runner directly because this test starts at the precise
    // state immediately after root:ivy:run has enqueued its request.
    let runner = Runner { host, ivy_loader, ivy_requests, io, runtime, root };

    let (stats, flags) = runner.normalize(false, 0, ());

    assert!(flags.success(), "dynamic Ivy execution failed:\nflags: {flags:#?}\nstats: {stats:#?}",);
  }
}
