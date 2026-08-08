use std::{
  mem::take,
  sync::{Arc, Mutex},
};

use ivy::name::Table;
use vine_util::register::Register;

use crate::{
  host::{
    Host,
    ext::{ExtFn, ExtOutput, FromRegister},
    module::{GraftHandle, ProgramHandle},
  },
  runtime::{
    Runtime,
    ext::ExtVal,
    port::{Port, Tag},
    wire::Wire,
  },
};

pub enum DynamicProgramRequest<'ivm> {
  ParseIvy { source: String, output: Wire<'ivm> },
  Resolve { module: ProgramHandle<'ivm>, entry: String, output: Wire<'ivm> },
}

#[derive(Clone, Default)]
pub struct DynamicProgramMailbox<'ivm> {
  inner: Arc<Mutex<Vec<DynamicProgramRequest<'ivm>>>>,
}

impl<'ivm> DynamicProgramMailbox<'ivm> {
  pub fn push(&self, request: DynamicProgramRequest<'ivm>) {
    self.inner.lock().unwrap().push(request);
  }

  pub fn drain(&self) -> Vec<DynamicProgramRequest<'ivm>> {
    take(&mut *self.inner.lock().unwrap())
  }

  pub fn extrinsics(&self) -> impl Register<Host<'ivm>> + use<'ivm> {
    let compile_requests = self.clone();
    let resolve_requests = self.clone();

    (
      ExtFn("root:ivy:parse", move |_host: &mut Host<'ivm>, _table: &mut Table| {
        move |_rt: &mut Runtime<'ivm, '_>, source: String, [output]: [Wire<'ivm>; 1]| {
          compile_requests.push(DynamicProgramRequest::ParseIvy { source, output });
        }
      }),
      ExtFn("root:ivm:program:resolve", move |_host: &mut Host<'ivm>, _table: &mut Table| {
        move |_rt: &mut Runtime<'ivm, '_>,
              (module, entry): (ProgramHandle<'ivm>, String),
              [output]: [Wire<'ivm>; 1]| {
          resolve_requests.push(DynamicProgramRequest::Resolve { module, entry, output });
        }
      }),
      ExtFn("root:ivm:graft:invoke", move |host: &mut Host<'ivm>, _table: &mut Table| {
        host.register_ext_ty::<GraftHandle>();

        move |rt: &mut Runtime<'ivm, '_>,
              entry: GraftHandle<'ivm>,
              [input, output]: [Wire<'ivm>; 2]| {
          let boundary = unsafe { rt.new_node(Tag::Comb, 0) };

          rt.link_wire_wire(boundary.1, input);
          rt.link_wire_wire(boundary.2, output);

          rt.link(Port::new_graft(entry.graft()), boundary.0);
        }
      }),
    )
  }
}

type ResolveResult<'ivm> = Result<GraftHandle<'ivm>, String>;
type CompileResult<'ivm> = Result<ProgramHandle<'ivm>, String>;

type ResolveResultEncoder<'ivm> =
  Box<dyn Fn(&mut Runtime<'ivm, '_>, ResolveResult<'ivm>) -> ExtVal<'ivm> + Send + Sync + 'ivm>;

type CompileResultEncoder<'ivm> =
  Box<dyn Fn(&mut Runtime<'ivm, '_>, CompileResult<'ivm>) -> ExtVal<'ivm> + Send + Sync + 'ivm>;

pub(crate) struct DynamicProgramService<'ivm> {
  requests: DynamicProgramMailbox<'ivm>,

  encode_compile_result: CompileResultEncoder<'ivm>,
  encode_resolve_result: ResolveResultEncoder<'ivm>,
}

impl<'ivm> DynamicProgramService<'ivm> {
  pub fn new(host: &mut Host<'ivm>, table: &mut Table) -> Self {
    let requests = DynamicProgramMailbox::default();

    host.register(table, requests.extrinsics());

    let encode_compile_result = Box::new(<CompileResult<'ivm> as ExtOutput<
      'ivm,
      Result<FromRegister, ()>,
    >>::register(host, table));

    let encode_resolve_result = Box::new(<ResolveResult<'ivm> as ExtOutput<
      'ivm,
      Result<FromRegister, ()>,
    >>::register(host, table));

    Self { requests, encode_compile_result, encode_resolve_result }
  }

  pub fn push(&self, request: DynamicProgramRequest<'ivm>) {
    self.requests.push(request);
  }

  pub fn drain(&self) -> Vec<DynamicProgramRequest<'ivm>> {
    self.requests.drain()
  }

  pub fn write_compile_result(
    &self,
    runtime: &mut Runtime<'ivm, '_>,
    output: Wire<'ivm>,
    result: CompileResult<'ivm>,
  ) {
    let result = (self.encode_compile_result)(runtime, result);
    runtime.link_wire(output, Port::new_ext_val(result));
  }

  pub fn write_resolve_result(
    &self,
    runtime: &mut Runtime<'ivm, '_>,
    output: Wire<'ivm>,
    result: ResolveResult<'ivm>,
  ) {
    let result = (self.encode_resolve_result)(runtime, result);
    runtime.link_wire(output, Port::new_ext_val(result));
  }
}
