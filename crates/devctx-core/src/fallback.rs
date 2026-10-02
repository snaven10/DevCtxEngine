//! Run-time CUDA → CPU fallback shared by the local embedder and reranker.
//!
//! Registering the CUDA execution provider can succeed and inference still fail
//! (e.g. an ONNX Runtime with no kernels for the GPU's architecture). A
//! [`CudaFallback`] holds the model and, on the first error while it is on CUDA,
//! rebuilds it on CPU, swaps it in and retries once. After that it stays on CPU.

use std::fmt::Display;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

type Rebuild<M, E> = Box<dyn Fn() -> Result<M, E> + Send + Sync>;

/// A model that drops from CUDA to CPU, once, if inference fails on CUDA.
pub struct CudaFallback<M, E> {
    /// What the model is for, e.g. `embeddings`; used in the warning.
    what: &'static str,
    model: RwLock<Arc<M>>,
    on_cuda: AtomicBool,
    /// Builds the same model on CPU.
    rebuild_on_cpu: Rebuild<M, E>,
}

impl<M, E: Display> CudaFallback<M, E> {
    /// `on_cuda` is whether `model` actually runs on CUDA; when false, [`run`]
    /// just calls through.
    ///
    /// [`run`]: Self::run
    pub fn new(
        what: &'static str,
        model: M,
        on_cuda: bool,
        rebuild_on_cpu: impl Fn() -> Result<M, E> + Send + Sync + 'static,
    ) -> Self {
        Self {
            what,
            model: RwLock::new(Arc::new(model)),
            on_cuda: AtomicBool::new(on_cuda),
            rebuild_on_cpu: Box::new(rebuild_on_cpu),
        }
    }

    /// Call `f` on the model. If that fails while on CUDA, warn once, move to
    /// CPU and call `f` again; that second result is returned as is.
    pub fn run<R>(&self, f: impl Fn(&M) -> Result<R, E>) -> Result<R, E> {
        let model = self.current();
        match f(&model) {
            Err(e) if self.on_cuda.load(Ordering::Acquire) => {
                self.switch_to_cpu(&e)?;
                f(&self.current())
            }
            // Another thread switched to CPU while this call was still running
            // on the CUDA model: its error is CUDA's, so retry on the new one.
            Err(_) if !Arc::ptr_eq(&model, &self.current()) => f(&self.current()),
            result => result,
        }
    }

    fn current(&self) -> Arc<M> {
        let guard = self.model.read().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(&guard)
    }

    /// Rebuild on CPU unless another thread already did.
    fn switch_to_cpu(&self, cause: &E) -> Result<(), E> {
        let mut slot = self.model.write().unwrap_or_else(PoisonError::into_inner);
        if self.on_cuda.swap(false, Ordering::AcqRel) {
            eprintln!(
                "devctx: warning: CUDA failed at run time for {} ({cause}); \
                 switching to CPU for the rest of this process",
                self.what
            );
            *slot = Arc::new((self.rebuild_on_cpu)()?);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Stand-in model: `Cuda` always errors, `Cpu` answers.
    #[derive(Debug, PartialEq)]
    enum Fake {
        Cuda,
        Cpu,
    }

    fn call(m: &Fake) -> Result<&'static str, String> {
        match m {
            Fake::Cuda => Err("no kernel image".into()),
            Fake::Cpu => Ok("cpu"),
        }
    }

    #[test]
    fn falls_back_once_then_stays_on_cpu() {
        let rebuilds = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&rebuilds);
        let fb = CudaFallback::new("test", Fake::Cuda, true, move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(Fake::Cpu)
        });
        assert_eq!(fb.run(call), Ok("cpu"));
        assert_eq!(fb.run(call), Ok("cpu"));
        assert_eq!(rebuilds.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cpu_failure_is_returned_without_retry() {
        let calls = AtomicUsize::new(0);
        let fb = CudaFallback::<Fake, String>::new("test", Fake::Cpu, false, || {
            panic!("must not rebuild on CPU")
        });
        let r: Result<(), String> = fb.run(|_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err("boom".into())
        });
        assert_eq!(r, Err("boom".to_string()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn call_in_flight_when_another_switches_is_retried() {
        let fb = CudaFallback::new("test", Fake::Cuda, true, || Ok(Fake::Cpu));
        // While this call runs on the CUDA model, a concurrent call fails and
        // switches to CPU; this one's CUDA error must not reach the caller.
        let r = fb.run(|m| {
            if *m == Fake::Cuda {
                assert_eq!(fb.run(call), Ok("cpu"));
            }
            call(m)
        });
        assert_eq!(r, Ok("cpu"));
    }

    #[test]
    fn failing_cpu_retry_returns_its_error() {
        let fb = CudaFallback::new("test", Fake::Cuda, true, || Ok(Fake::Cuda));
        assert_eq!(fb.run(call), Err("no kernel image".to_string()));
        // Already switched: no further rebuild, plain call-through.
        assert_eq!(fb.run(call), Err("no kernel image".to_string()));
    }
}
