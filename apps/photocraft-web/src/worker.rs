//! Isolated MobileSAM browser worker.
//!
//! Trunk emits the loader shim and wasm-bindgen glue. This binary owns model
//! bytes and inference state; it has no document, history, DOM or editor APIs.

#[cfg(target_arch = "wasm32")]
mod web_worker {
    use std::cell::RefCell;
    use std::rc::Rc;

    use photocraft_ml::mobile_sam::MobileSamBackend;
    use photocraft_ml::{SegmentationBackend as _, WORKER_PROTOCOL_VERSION, WorkerRequest, WorkerResponse};
    use wasm_bindgen::{JsCast as _, closure::Closure};

    const MODEL_PARTS: [&str; 5] = [
        "models/mobile-sam-tiny-vitt.safetensors.part-aa",
        "models/mobile-sam-tiny-vitt.safetensors.part-ab",
        "models/mobile-sam-tiny-vitt.safetensors.part-ac",
        "models/mobile-sam-tiny-vitt.safetensors.part-ad",
        "models/mobile-sam-tiny-vitt.safetensors.part-ae",
    ];
    const MAX_LOADING_QUEUE: usize = 4;

    enum State {
        Loading(Vec<WorkerRequest>),
        Ready(MobileSamBackend),
        Failed(String),
    }

    type SharedState = Rc<RefCell<State>>;

    pub fn start() {
        let scope = scope();
        let state = Rc::new(RefCell::new(State::Loading(Vec::new())));
        let requests = state.clone();
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let request = match serde_wasm_bindgen::from_value::<WorkerRequest>(event.data()) {
                Ok(request) if request.version == WORKER_PROTOCOL_VERSION => request,
                Ok(request) => {
                    respond(WorkerResponse {
                        version: WORKER_PROTOCOL_VERSION,
                        id: request.id,
                        result: Err(format!("Unsupported object-worker protocol {}", request.version)),
                    });
                    return;
                }
                Err(error) => {
                    web_sys::console::error_1(&format!("photocraft object worker: invalid request: {error}").into());
                    return;
                }
            };
            dispatch(&requests, request);
        });
        scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        on_message.forget();

        wasm_bindgen_futures::spawn_local(async move {
            let loaded = load_model().await;
            let queued = {
                let mut state_ref = state.borrow_mut();
                let queued = match &mut *state_ref {
                    State::Loading(queued) => std::mem::take(queued),
                    State::Ready(_) | State::Failed(_) => Vec::new(),
                };
                *state_ref = match loaded {
                    Ok(model) => State::Ready(model),
                    Err(error) => State::Failed(error),
                };
                queued
            };
            for request in queued {
                dispatch(&state, request);
            }
        });
    }

    fn dispatch(state: &SharedState, request: WorkerRequest) {
        let id = request.id;
        let result = {
            let mut state_ref = state.borrow_mut();
            match &mut *state_ref {
                State::Loading(queued) if queued.len() < MAX_LOADING_QUEUE => {
                    queued.push(request);
                    return;
                }
                State::Loading(_) => Err("The local object model is loading and its request queue is full".into()),
                State::Ready(model) => model.segment(request.input.borrowed()).map_err(|error| error.to_string()),
                State::Failed(error) => Err(format!("The local object model could not load: {error}")),
            }
        };
        respond(WorkerResponse { version: WORKER_PROTOCOL_VERSION, id, result });
    }

    fn respond(response: WorkerResponse) {
        match serde_wasm_bindgen::to_value(&response) {
            Ok(value) => {
                if let Err(error) = scope().post_message(&value) {
                    web_sys::console::error_1(&format!("photocraft object worker: could not post response: {error:?}").into());
                }
            }
            Err(error) => web_sys::console::error_1(&format!("photocraft object worker: could not encode response: {error}").into()),
        }
    }

    async fn load_model() -> Result<MobileSamBackend, String> {
        let mut weights = Vec::with_capacity(photocraft_ml::MOBILE_SAM_BYTES);
        for part in MODEL_PARTS {
            weights.extend(fetch_bytes(part).await?);
        }
        MobileSamBackend::from_safetensors(weights).map_err(|error| error.to_string())
    }

    async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
        use wasm_bindgen_futures::JsFuture;
        let js = |error: wasm_bindgen::JsValue| format!("{error:?}");
        let response: web_sys::Response = JsFuture::from(scope().fetch_with_str(url)).await.map_err(js)?.dyn_into().map_err(|_| "not a Response")?;
        if !response.ok() {
            return Err(format!("HTTP {} loading {url}", response.status()));
        }
        let body = JsFuture::from(response.array_buffer().map_err(js)?).await.map_err(js)?;
        Ok(js_sys::Uint8Array::new(&body).to_vec())
    }

    fn scope() -> web_sys::DedicatedWorkerGlobalScope {
        js_sys::global().unchecked_into()
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    web_worker::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {}
