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
      common::{Nil, Pair},
    },
    module::IvyModule,
  },
  runtime::{
    Runtime,
    ext::{ExtTy, ExtVal},
    port::Port,
    wire::Wire,
  },
};

pub enum IvyRequest<'ivm> {
  Compile { source: String, output: Wire<'ivm> },
  Run { module: IvyModule<'ivm>, entry: String, input: ExtVal<'ivm>, result_output: Wire<'ivm> },
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
    let compile_requests = self.clone();
    let run_requests = self.clone();

    (
      ExtFn("root:ivy:module:compile", move |_host: &mut Host<'ivm>, _table: &mut Table| {
        move |_rt: &mut Runtime<'ivm, '_>, source: String, [output]: [Wire<'ivm>; 1]| {
          compile_requests.push(IvyRequest::Compile { source, output });
        }
      }),
      ExtFn("root:ivy:module:run", move |_host: &mut Host<'ivm>, _table: &mut Table| {
        move |_rt: &mut Runtime<'ivm, '_>,
              (module, entry, input): (IvyModule<'ivm>, String, ExtVal<'ivm>),
              [result_output]: [Wire<'ivm>; 1]| {
          run_requests.push(IvyRequest::Run { module, entry, input, result_output });
        }
      }),
    )
  }
}

fn finish_ok<'ivm>() -> impl Register<Host<'ivm>> {
  ExtFn("root:ivy:finish_ok", |host: &mut Host<'ivm>, _: &mut Table| {
    let n32 = host.register_ext_ty::<u32>();
    let pair = host.register_ext_ty::<Pair>();

    move |rt: &mut Runtime<'ivm, '_>, output: ExtVal<'ivm>, [result_output]: [Wire<'ivm>; 1]| {
      let result = pair.wrap(
        rt,
        Pair(
          n32.wrap_static(1), // Result::Ok
          output,
        ),
      );

      rt.link_wire(result_output, Port::new_ext_val(result));
    }
  })
}

type CompileResult<'ivm> = Result<IvyModule<'ivm>, String>;
type LoadResult = Result<Nil, String>;

type CompileResultEncoder<'ivm> =
  Box<dyn Fn(&mut Runtime<'ivm, '_>, CompileResult<'ivm>) -> ExtVal<'ivm> + Send + Sync + 'ivm>;

type UpdateResultEncoder<'ivm> =
  Box<dyn Fn(&mut Runtime<'ivm, '_>, LoadResult) -> ExtVal<'ivm> + Send + Sync + 'ivm>;

type StringEncoder<'ivm> =
  Box<dyn Fn(&mut Runtime<'ivm, '_>, String) -> ExtVal<'ivm> + Send + Sync + 'ivm>;

pub(crate) struct IvyService<'ivm> {
  requests: IvyRequests<'ivm>,

  finish_ok_label: u16,

  encode_compile_result: CompileResultEncoder<'ivm>,

  encode_string: StringEncoder<'ivm>,
  n32: ExtTy<'ivm, u32>,
  pair: ExtTy<'ivm, Pair<'ivm>>,
}

impl<'ivm> IvyService<'ivm> {
  pub fn new(host: &mut Host<'ivm>, table: &mut Table) -> Self {
    let requests = IvyRequests::default();

    host.register(table, (requests.extrinsics(), finish_ok()));

    let finish_ok_name = table.add_path_name("root:ivy:finish_ok");

    let finish_ok_label = host
      .ext_split_lookup
      .get(&finish_ok_name)
      .expect("root:ivy:finish_ok was just registered")
      .bits();

    // This encodes Rust Result<Nil, String> as Vine Result[(), String].
    let encode_update_result =
      Box::new(<LoadResult as ExtOutput<'ivm, Result<FromRegister, ()>>>::register(host, table));

    let encode_string = Box::new(<String as ExtOutput<'ivm, ()>>::register(host, table));

    let n32 = host.register_ext_ty::<u32>();
    let pair = host.register_ext_ty::<Pair>();

    let encode_compile_result = Box::new(<CompileResult<'ivm> as ExtOutput<
      'ivm,
      Result<FromRegister, ()>,
    >>::register(host, table));

    Self { requests, finish_ok_label, encode_compile_result, encode_string, n32, pair }
  }

  pub fn push(&self, request: IvyRequest<'ivm>) {
    self.requests.push(request);
  }

  pub fn drain(&self) -> Vec<IvyRequest<'ivm>> {
    self.requests.drain()
  }

  pub fn write_run_error(
    &self,
    runtime: &mut Runtime<'ivm, '_>,
    result_output: Wire<'ivm>,
    error: String,
    input: ExtVal<'ivm>,
  ) {
    let error = (self.encode_string)(runtime, error);

    // Payload of the Err variant: (LoadError, I)
    let error_and_input = self.pair.wrap(runtime, Pair(error, input));

    let result = self.pair.wrap(runtime, Pair(self.n32.wrap_static(0), error_and_input));

    runtime.link_wire(result_output, Port::new_ext_val(result));
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

  pub(crate) fn finish_ok_label(&self) -> u16 {
    self.finish_ok_label
  }
}
