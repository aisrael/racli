pub mod transport;

use jsonrpsee::async_client::Client;
use jsonrpsee::async_client::ClientBuilder;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::client::Subscription;
use jsonrpsee::core::client::SubscriptionClientT;
use jsonrpsee::core::client::TransportReceiverT;
use jsonrpsee::core::client::TransportSenderT;
use jsonrpsee::core::traits::ToRpcParams;
use lsp_types::notification::*;
use lsp_types::request::*;
use lsp_types::*;
use serde::Serialize;
use serde_json::value::RawValue;

struct SerdeParam<T>(T)
where
    T: Serialize;

impl<T> ToRpcParams for SerdeParam<T>
where
    T: Serialize,
{
    fn to_rpc_params(self) -> Result<Option<Box<RawValue>>, serde_json::Error> {
        let json = serde_json::to_string(&self.0)?;
        RawValue::from_string(json).map(Some)
    }
}

/// Untyped JSON params; `None` omits `params` from the JSON-RPC message.
struct RawParams(Option<serde_json::Value>);

impl ToRpcParams for RawParams {
    fn to_rpc_params(self) -> Result<Option<Box<RawValue>>, serde_json::Error> {
        self.0
            .map(|v| serde_json::value::to_raw_value(&v))
            .transpose()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum LspError {
    #[error("jsonrpsee error: {0}")]
    Jsonrpsee(#[from] jsonrpsee::core::client::Error),
}

/// A client for the Language Server Protocol.
#[derive(Debug)]
pub struct LspClient {
    client: Client,
}

impl LspClient {
    pub fn new<S, R>(sender: S, receiver: R) -> Self
    where
        S: TransportSenderT + Send,
        R: TransportReceiverT + Send,
    {
        let client = ClientBuilder::default().build_with_tokio(sender, receiver);
        Self { client }
    }

    /// Request the server to initialize the client.
    ///
    /// <https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#initialize>
    pub async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult, LspError> {
        self.send_request::<Initialize>(params).await
    }

    /// Notify the server that the client received the result of the `initialize` request.
    ///
    /// <https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#initialized>
    pub async fn initialized(&self) -> Result<(), LspError> {
        let params = InitializedParams {};
        self.send_notification::<Initialized>(params).await
    }

    /// Request the server to shutdown the client.
    ///
    /// <https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#shutdown>
    pub async fn shutdown(&self) -> Result<(), LspError> {
        self.send_request::<Shutdown>(()).await
    }

    /// Notify the server to exit the process.
    ///
    /// <https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#exit>
    pub async fn exit(&self) -> Result<(), LspError> {
        self.send_notification::<Exit>(()).await
    }

    /// Send an LSP request to the server.
    pub async fn send_request<R>(&self, params: R::Params) -> Result<R::Result, LspError>
    where
        R: Request,
    {
        let result = self.client.request(R::METHOD, SerdeParam(params)).await?;
        Ok(result)
    }

    /// Send an LSP notification to the server.
    pub async fn send_notification<N>(&self, params: N::Params) -> Result<(), LspError>
    where
        N: Notification,
    {
        self.client
            .notification(N::METHOD, SerdeParam(params))
            .await?;
        Ok(())
    }

    /// Send an untyped LSP request and return the raw JSON `result`.
    pub async fn request_raw(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, LspError> {
        Ok(self.client.request(method, RawParams(params)).await?)
    }

    /// Send an untyped LSP notification.
    pub async fn notify_raw(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), LspError> {
        Ok(self.client.notification(method, RawParams(params)).await?)
    }

    /// Create an untyped subscription to server notifications named `method`.
    pub async fn subscribe_raw(
        &self,
        method: &str,
    ) -> Result<Subscription<serde_json::Value>, LspError> {
        Ok(self.client.subscribe_to_method(method).await?)
    }

    /// Create a subscription to an LSP notification.
    pub async fn subscribe_to_method<N>(&self) -> Result<Subscription<N::Params>, LspError>
    where
        N: Notification,
    {
        let subscription = self.client.subscribe_to_method(N::METHOD).await?;
        Ok(subscription)
    }
}
