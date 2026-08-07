use std::{
  mem::take,
  sync::{Arc, Mutex},
};

use ivy::name::Table;
use vine_util::register::Register;

use crate::{
  host::{
    Host,
    ext::{
      ExtFn, ExtOutput, FromRegister,
      common::{IO, Nil},
      error,
    },
  },
  runtime::{Runtime, ext::ExtVal, port::Port, wire::Wire},
};

pub struct IvyRequest<'ivm> {
  pub(crate) source: String,
  pub(crate) io: ExtVal<'ivm>,
  pub(crate) result_output: Wire<'ivm>,
  pub(crate) io_output: Wire<'ivm>,
}

#[derive(Clone, Default)]
pub struct IvyRequests<'ivm> {
  inner: Arc<Mutex<Vec<IvyRequest<'ivm>>>>,
}

impl<'ivm> IvyRequests<'ivm> {
  pub fn push(&self, request: IvyRequest<'ivm>) {
    self.inner.lock().unwrap().push(request);
  }

  pub fn drain(&self) -> Vec<IvyRequest<'ivm>> {
    take(&mut *self.inner.lock().unwrap())
  }

  pub fn extrinsics(&self) -> impl Register<Host<'ivm>> + use<'ivm> {
    let requests = self.clone();

    ExtFn("root:ivy:run", move |host: &mut Host<'ivm>, _: &mut Table| {
      let io_ty = host.register_ext_ty::<IO>();

      move |rt: &mut Runtime<'ivm, '_>,
            (io, source): (ExtVal<'ivm>, String),
            [result_output, io_output]: [Wire<'ivm>; 2]| {
        if io.ty_id() != io_ty.id() {
          return error(rt, [result_output, io_output]);
        }

        requests.push(IvyRequest { source, io, result_output, io_output });
      }
    })
  }
}

type LoadResult = Result<Nil, String>;

type ResultEncoder<'ivm> =
  Box<dyn Fn(&mut Runtime<'ivm, '_>, LoadResult) -> ExtVal<'ivm> + Send + Sync + 'ivm>;

pub(crate) struct IvyService<'ivm> {
  requests: IvyRequests<'ivm>,
  encode_result: ResultEncoder<'ivm>,
}

impl<'ivm> IvyService<'ivm> {
  pub fn new(host: &mut Host<'ivm>, table: &mut Table) -> Self {
    let requests = IvyRequests::default();

    // This encodes Rust Result<Nil, String> as Vine Result[(), String].
    let encode_result =
      <LoadResult as ExtOutput<'ivm, Result<FromRegister, ()>>>::register(host, table);

    host.register(table, requests.extrinsics());

    Self { requests, encode_result: Box::new(encode_result) }
  }

  pub fn push(&self, request: IvyRequest<'ivm>) {
    self.requests.push(request);
  }

  pub fn drain(&self) -> Vec<IvyRequest<'ivm>> {
    self.requests.drain()
  }

  pub fn write_results(
    &self,
    runtime: &mut Runtime<'ivm, '_>,
    output: Wire<'ivm>,
    result: Result<Nil, String>,
  ) {
    let value = (self.encode_result)(runtime, result);
    runtime.link_wire(output, Port::new_ext_val(value));
  }
}
