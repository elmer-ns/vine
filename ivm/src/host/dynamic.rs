use std::{
  mem::take,
  sync::{Arc, Mutex},
};

use ivy::name::Table;
use vine_util::register::Register;

use crate::{
  host::{
    Host,
    ext::{ExtFn, common::IO, error},
  },
  runtime::{Runtime, ext::ExtVal, wire::Wire},
};

pub(crate) struct IvyRequest<'ivm> {
  pub(crate) source: String,
  pub(crate) io: ExtVal<'ivm>,
  pub(crate) output: Wire<'ivm>,
}

#[derive(Clone)]
pub(crate) struct IvyRequests<'ivm> {
  inner: Arc<Mutex<Vec<IvyRequest<'ivm>>>>,
}

impl<'ivm> Default for IvyRequests<'ivm> {
  fn default() -> Self {
    Self { inner: Arc::new(Mutex::new(Vec::new())) }
  }
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
            [output]: [Wire<'ivm>; 1]| {
        if io.ty_id() != io_ty.id() {
          return error(rt, [output]);
        }

        requests.push(IvyRequest { source, io, output });
      }
    })
  }
}
