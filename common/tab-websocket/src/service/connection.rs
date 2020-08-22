use crate::{
    bus::WebsocketConnectionBus,
    message::connection::{WebsocketRecv, WebsocketSend},
    resource::connection::WebsocketResource,
};
use log::debug;
use tab_service::{Bus, Lifeline, Service};
use tokio::{select, sync::mpsc};

use crate::common::{self, should_terminate};

use futures::{SinkExt, StreamExt};
use log::{error, trace};

use anyhow::Context;
use std::fmt::Debug;
use tab_service::{
    ResourceTakenError, ResourceUninitializedError, TakeChannelError, TakeResourceError,
};
use thiserror::Error;
use tungstenite::Error;

#[derive(Debug)]
pub struct WebsocketService {
    _runloop: Lifeline,
}

impl Service for WebsocketService {
    type Bus = WebsocketConnectionBus;
    type Lifeline = Result<Self, WebsocketSpawnError>;

    fn spawn(bus: &WebsocketConnectionBus) -> Result<Self, WebsocketSpawnError> {
        // let mut websocket = parse_bincode(websocket);

        // let (mut tx_request, rx_request) = mpsc::channel::<Request>(4);
        // let (tx_response, mut rx_response) = mpsc::channel::<Response>(4);
        let websocket = bus
            .resource::<WebsocketResource>()
            .map_err(WebsocketSpawnError::socket_error)?;

        let rx = bus
            .rx::<WebsocketSend>()
            .map_err(WebsocketSpawnError::bus_failure)?;

        let tx = bus
            .tx::<WebsocketRecv>()
            .map_err(WebsocketSpawnError::bus_failure)?;

        let _runloop = Self::try_task("run", runloop(websocket, rx, tx));

        Ok(Self { _runloop })
    }
}

async fn runloop(
    mut websocket_drop: WebsocketResource,
    mut rx: mpsc::Receiver<WebsocketSend>,
    mut tx: mpsc::Sender<WebsocketRecv>,
) -> anyhow::Result<()> {
    let websocket = &mut websocket_drop.0;
    loop {
        select!(
            message = websocket.next() => {
                if let None = message {
                    debug!("terminating - websocket disconnected");
                    break;
                }

                trace!("message received: {:?}", &message);

                let message = message.unwrap();
                if let Err(e) = message {
                    match e {
                        Error::ConnectionClosed | Error::AlreadyClosed | Error::Protocol(_)=> {
                            break;
                        },
                        _ => {
                            error!("message error: {}", e);
                            continue;
                        }
                    }
                }

                let message = message.unwrap();
                if should_terminate(&message) {
                    debug!("terminating - received close");
                    break;
                }

                tx.send(WebsocketRecv(message)).await.context("send WebsocketRecv")?;
            },
            message = rx.recv() => {
                if !message.is_some()  {
                    common::send_close(websocket).await;

                    debug!("terminating - channel disconnected");
                    break;
                }

                let message = message.unwrap();

                trace!("send message: {:?}", &message);
                websocket.send(message.0).await.context("wire send Tungstenite::Message")?;
            },
        );
    }

    debug!("server loop terminated");
    Ok(())
}
#[derive(Error, Debug)]
pub enum WebsocketSpawnError {
    #[error("socket taken: {0}")]
    SocketTaken(ResourceTakenError),

    #[error("socket uninitialized: {0}")]
    SocketUninitialized(ResourceUninitializedError),

    #[error("websocket channel taken: {0}")]
    BusFailure(TakeChannelError),
}

impl WebsocketSpawnError {
    pub fn socket_error(err: TakeResourceError) -> Self {
        match err {
            TakeResourceError::Uninitialized(uninit) => Self::SocketUninitialized(uninit),
            TakeResourceError::Taken(taken) => Self::SocketTaken(taken),
        }
    }

    pub fn bus_failure(err: TakeChannelError) -> Self {
        Self::BusFailure(err)
    }
}