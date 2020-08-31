use crate::prelude::*;
use crate::{
    message::{
        cli::{CliRecv, CliSend, CliShutdown},
        listener::ListenerShutdown,
        tab::{TabInput, TabRecv, TabSend},
        tab_manager::{TabManagerRecv, TabManagerSend},
    },
    state::tab::TabsState,
};

use anyhow::Context;
use lifeline::{subscription, Resource};
use std::sync::Arc;
use subscription::Subscription;
use tab_api::{chunk::OutputChunk, client::Request, client::Response, tab::TabId};
use tab_websocket::{bus::WebsocketMessageBus, resource::connection::WebsocketResource};
use time::Duration;
use tokio::{
    sync::{broadcast, mpsc},
    time,
};

lifeline_bus!(pub struct CliBus);

impl Message<CliBus> for CliShutdown {
    type Channel = mpsc::Sender<Self>;
}

impl Message<CliBus> for Request {
    type Channel = broadcast::Sender<Self>;
}

impl Message<CliBus> for Response {
    type Channel = broadcast::Sender<Self>;
}

impl Message<CliBus> for CliSend {
    type Channel = mpsc::Sender<Self>;
}

impl Message<CliBus> for CliRecv {
    type Channel = mpsc::Sender<Self>;
}

impl Message<CliBus> for subscription::Subscription<TabId> {
    type Channel = subscription::Sender<TabId>;
}

impl Message<CliBus> for TabsState {
    type Channel = mpsc::Sender<Self>;
}

impl Resource<CliBus> for WebsocketResource {}
impl WebsocketMessageBus for CliBus {
    type Send = Response;
    type Recv = Request;
}

pub struct ListenerConnectionCarrier {
    _forward: Lifeline,
    _reverse: Lifeline,
    _terminated: Lifeline,
    _forward_tabs_state: Lifeline,
}

impl CarryFrom<ListenerBus> for CliBus {
    type Lifeline = anyhow::Result<ListenerConnectionCarrier>;

    fn carry_from(&self, from: &ListenerBus) -> Self::Lifeline {
        let _forward = {
            let rx_tab = from.rx::<TabSend>()?;
            let id_subscription = self.rx::<Subscription<TabId>>()?.into_inner();

            let tx_conn = self.tx::<CliRecv>()?;

            Self::try_task(
                "output",
                Self::run_output(rx_tab, tx_conn.clone(), id_subscription),
            )
        };

        let _reverse = {
            let rx_conn = self.rx::<CliSend>()?;

            let tx_tab = from.tx::<TabRecv>()?;
            let tx_manager = from.tx::<TabManagerRecv>()?;
            let tx_shutdown = self.tx::<CliShutdown>()?;
            let tx_listener_shutdown = from.tx::<ListenerShutdown>()?;
            Self::try_task(
                "input",
                Self::run_input(
                    rx_conn,
                    tx_tab,
                    tx_manager,
                    tx_shutdown,
                    tx_listener_shutdown,
                ),
            )
        };

        let _terminated = {
            let rx_manager = from.rx::<TabManagerSend>()?;
            let tx_conn = self.tx::<CliRecv>()?;
            Self::try_task("terminated", Self::handle_terminated(rx_manager, tx_conn))
        };

        let _forward_tabs_state = {
            let mut rx_tabs_state = from.rx::<TabsState>()?;
            let mut tx_tabs_state = self.tx::<TabsState>()?;
            Self::try_task("forward_tabs_state", async move {
                while let Some(msg) = rx_tabs_state.recv().await {
                    tx_tabs_state.send(msg).await?;
                }

                Ok(())
            })
        };

        Ok(ListenerConnectionCarrier {
            _forward,
            _reverse,
            _terminated,
            _forward_tabs_state,
        })
    }
}

impl CliBus {
    async fn run_output(
        mut rx: impl Receiver<TabSend>,
        mut tx: impl Sender<CliRecv>,
        id_subscription: subscription::Receiver<TabId>,
    ) -> anyhow::Result<()> {
        while let Some(msg) = rx.recv().await {
            Self::handle_tabsend(msg, &mut tx, &id_subscription).await?
        }

        Ok(())
    }

    async fn run_input(
        mut rx: impl Receiver<CliSend>,
        mut tx: impl Sender<TabRecv>,
        mut tx_manager: impl Sender<TabManagerRecv>,
        mut tx_shutdown: impl Sender<CliShutdown>,
        mut tx_listener_shutdown: impl Sender<ListenerShutdown>,
    ) -> anyhow::Result<()> {
        while let Some(msg) = rx.recv().await {
            match msg {
                CliSend::CreateTab(create) => {
                    debug!("received CreateTab from client: {:?}", &create);
                    tx_manager.send(TabManagerRecv::CreateTab(create)).await?;
                }
                CliSend::CloseTab(id) => {
                    tx_manager.send(TabManagerRecv::CloseTab(id)).await?;
                }
                CliSend::CloseNamedTab(name) => {
                    tx_manager.send(TabManagerRecv::CloseNamedTab(name)).await?;
                }
                CliSend::RequestScrollback(id) => {
                    tx.send(TabRecv::Scrollback(id))
                        .await
                        .context("tx TabRecv::Scrollback")?;
                }
                CliSend::Input(id, input) => {
                    let stdin = Arc::new(input);
                    let input = TabInput { id, stdin };
                    let message = TabRecv::Input(input);
                    tx.send(message).await.context("tx TabRecv closed")?;
                }
                CliSend::ResizeTab(id, dimensions) => {
                    let message = TabRecv::Resize(id, dimensions);
                    tx.send(message).await?;
                }
                CliSend::Retask(from, to) => {
                    let message = TabRecv::Retask(from, to);
                    tx.send(message).await?;
                }
                CliSend::GlobalShutdown => {
                    info!("global shutdown received");
                    tx.send(TabRecv::TerminateAll).await?;
                    tx_listener_shutdown.send(ListenerShutdown {}).await?;
                    time::delay_for(Duration::from_millis(50)).await;
                }
            }
        }

        tx_shutdown
            .send(CliShutdown {})
            .await
            .context("tx ConnectionShutdown closed")?;

        Ok(())
    }

    async fn handle_terminated(
        mut rx: impl Receiver<TabManagerSend>,
        mut tx: impl Sender<CliRecv>,
    ) -> anyhow::Result<()> {
        while let Some(msg) = rx.recv().await {
            match msg {
                TabManagerSend::TabTerminated(id) => {
                    tx.send(CliRecv::TabStopped(id)).await?;
                }
            }
        }

        Ok(())
    }

    async fn handle_tabsend(
        msg: TabSend,
        tx: &mut impl Sender<CliRecv>,
        id_subscription: &subscription::Receiver<TabId>,
    ) -> anyhow::Result<()> {
        match msg {
            TabSend::Started(tab) => tx.send(CliRecv::TabStarted(tab)).await?,
            TabSend::Stopped(id) => {
                info!("notifying client of terminated tab {}", id);
                tx.send(CliRecv::TabStopped(id)).await?;
            }
            TabSend::Scrollback(scrollback) => {
                tx.send(CliRecv::Scrollback(scrollback)).await?;
            }
            TabSend::Output(stdout) => {
                if !id_subscription.contains(&stdout.id) {
                    return Ok(());
                }

                tx.send(CliRecv::Output(
                    stdout.id,
                    OutputChunk::clone(stdout.stdout.as_ref()),
                ))
                .await?
            }
            TabSend::Retask(from, to) => {
                if !id_subscription.contains(&from) {
                    return Ok(());
                }

                info!("retasking client from {:?} to {:?}", from, to);
                tx.send(CliRecv::Retask(from, to)).await?;
            }
        };

        Ok(())
    }
}

#[cfg(test)]
mod forward_tests {
    use crate::message::{
        cli::CliRecv,
        tab::{TabOutput, TabScrollback, TabSend},
    };
    use crate::{
        prelude::*, service::pty::scrollback::ScrollbackBuffer, state::pty::PtyScrollback,
    };
    use lifeline::{assert_completes, assert_times_out, subscription::Subscription};
    use std::sync::Arc;
    use tab_api::{
        chunk::OutputChunk,
        tab::{TabId, TabMetadata},
    };
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn started() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = listener_bus.tx::<TabSend>()?;
        let mut rx = cli_bus.rx::<CliRecv>()?;

        let started = TabMetadata {
            id: TabId(0),
            name: "name".into(),
            dimensions: (1, 1),
            shell: "bash".into(),
            dir: "dir".into(),
        };

        tx.send(TabSend::Started(started.clone())).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(CliRecv::TabStarted(started), msg.unwrap());
        });

        Ok(())
    }

    #[tokio::test]
    async fn stopped() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = listener_bus.tx::<TabSend>()?;
        let mut rx = cli_bus.rx::<CliRecv>()?;

        tx.send(TabSend::Stopped(TabId(0))).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(CliRecv::TabStopped(TabId(0)), msg.unwrap());
        });

        Ok(())
    }

    #[tokio::test]
    async fn scrollback() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = listener_bus.tx::<TabSend>()?;
        let mut rx = cli_bus.rx::<CliRecv>()?;

        let mut buffer = ScrollbackBuffer::new();
        buffer.push(OutputChunk {
            index: 0,
            data: vec![0, 1],
        });
        buffer.push(OutputChunk {
            index: 1,
            data: vec![1, 2],
        });
        let scrollback = PtyScrollback::new(Arc::new(Mutex::new(buffer)));
        let scrollback = TabScrollback {
            id: TabId(0),
            scrollback,
        };
        tx.send(TabSend::Scrollback(scrollback)).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            if let CliRecv::Scrollback(scroll) = msg.unwrap() {
                let mut iter = scroll.scrollback().await;
                assert_eq!(
                    Some(OutputChunk {
                        index: 1,
                        data: vec![0, 1, 1, 2]
                    }),
                    iter.next()
                );
                assert_eq!(None, iter.next());
            } else {
                panic!("Expected CliRecv::Scrollback, found None");
            }
        });

        Ok(())
    }

    #[tokio::test]
    async fn output_subscribed() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = listener_bus.tx::<TabSend>()?;
        let mut tx_subscription = cli_bus.tx::<Subscription<TabId>>()?;
        let mut rx = cli_bus.rx::<CliRecv>()?;

        tx_subscription
            .send(Subscription::Subscribe(TabId(0)))
            .await?;

        tx.send(TabSend::Output(TabOutput {
            id: TabId(0),
            stdout: Arc::new(OutputChunk {
                index: 0,
                data: vec![0, 1, 2],
            }),
        }))
        .await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            if let Some(CliRecv::Output(id, chunk)) = msg {
                assert_eq!(TabId(0), id);
                assert_eq!(
                    OutputChunk {
                        index: 0,
                        data: vec![0, 1, 2],
                    },
                    chunk
                );
            } else {
                panic!("expected CliRecv::Output, found: {:?}", msg)
            }
        });

        Ok(())
    }

    #[tokio::test]
    async fn output_unsubscribed() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = listener_bus.tx::<TabSend>()?;
        let mut rx = cli_bus.rx::<CliRecv>()?.into_inner();

        tx.send(TabSend::Output(TabOutput {
            id: TabId(0),
            stdout: Arc::new(OutputChunk {
                index: 0,
                data: vec![0, 1, 2],
            }),
        }))
        .await?;

        assert_times_out!(async move {
            rx.recv().await;
        });

        Ok(())
    }

    #[tokio::test]
    async fn retask_subscribed() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = listener_bus.tx::<TabSend>()?;
        let mut tx_subscription = cli_bus.tx::<Subscription<TabId>>()?;
        let mut rx = cli_bus.rx::<CliRecv>()?;

        tx_subscription
            .send(Subscription::Subscribe(TabId(0)))
            .await?;
        tx.send(TabSend::Retask(TabId(0), TabId(1))).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(CliRecv::Retask(TabId(0), TabId(1)), msg.unwrap());
        });

        Ok(())
    }

    #[tokio::test]
    async fn retask_unsubscribed() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = listener_bus.tx::<TabSend>()?;
        let mut rx = cli_bus.rx::<CliRecv>()?;

        tx.send(TabSend::Retask(TabId(0), TabId(1))).await?;

        assert_times_out!(async move {
            let _msg = rx.recv().await;
        });

        Ok(())
    }
}

#[cfg(test)]
mod reverse_tests {
    use crate::{
        message::{
            cli::CliSend,
            listener::ListenerShutdown,
            tab::{TabInput, TabRecv},
            tab_manager::TabManagerRecv,
        },
        prelude::*,
    };
    use lifeline::assert_completes;
    use tab_api::{
        chunk::InputChunk,
        tab::{CreateTabMetadata, TabId},
    };

    #[tokio::test]
    async fn create_tab() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = cli_bus.tx::<CliSend>()?;
        let mut rx = listener_bus.rx::<TabManagerRecv>()?;

        let create = CreateTabMetadata {
            name: "name".into(),
            shell: "bash".into(),
            dimensions: (1, 1),
            dir: "dir".into(),
        };

        tx.send(CliSend::CreateTab(create.clone())).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(TabManagerRecv::CreateTab(create), msg.unwrap());
        });

        Ok(())
    }

    #[tokio::test]
    async fn close_tab() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = cli_bus.tx::<CliSend>()?;
        let mut rx = listener_bus.rx::<TabManagerRecv>()?;

        tx.send(CliSend::CloseTab(TabId(0))).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(TabManagerRecv::CloseTab(TabId(0)), msg.unwrap());
        });

        Ok(())
    }

    #[tokio::test]
    async fn close_named_tab() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = cli_bus.tx::<CliSend>()?;
        let mut rx = listener_bus.rx::<TabManagerRecv>()?;

        tx.send(CliSend::CloseNamedTab("foo".into())).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(TabManagerRecv::CloseNamedTab("foo".into()), msg.unwrap());
        });

        Ok(())
    }

    #[tokio::test]
    async fn request_scrollback() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = cli_bus.tx::<CliSend>()?;
        let mut rx = listener_bus.rx::<TabRecv>()?;

        tx.send(CliSend::RequestScrollback(TabId(0))).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(TabRecv::Scrollback(TabId(0)), msg.unwrap());
        });

        Ok(())
    }

    #[tokio::test]
    async fn input() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = cli_bus.tx::<CliSend>()?;
        let mut rx = listener_bus.rx::<TabRecv>()?;

        tx.send(CliSend::Input(TabId(0), InputChunk { data: vec![0] }))
            .await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(
                TabRecv::Input(TabInput::new(TabId(0), vec![0u8])),
                msg.unwrap()
            );
        });

        Ok(())
    }

    #[tokio::test]
    async fn resize() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = cli_bus.tx::<CliSend>()?;
        let mut rx = listener_bus.rx::<TabRecv>()?;

        tx.send(CliSend::ResizeTab(TabId(0), (1, 2))).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(TabRecv::Resize(TabId(0), (1, 2)), msg.unwrap());
        });

        Ok(())
    }

    #[tokio::test]
    async fn retask() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = cli_bus.tx::<CliSend>()?;
        let mut rx = listener_bus.rx::<TabRecv>()?;

        tx.send(CliSend::Retask(TabId(0), TabId(1))).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(TabRecv::Retask(TabId(0), TabId(1)), msg.unwrap());
        });

        Ok(())
    }

    #[tokio::test]
    async fn global_shutdown() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = cli_bus.tx::<CliSend>()?;
        let mut rx = listener_bus.rx::<ListenerShutdown>()?;

        tx.send(CliSend::GlobalShutdown).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
        });

        Ok(())
    }
}

#[cfg(test)]
mod terminated_tests {
    use crate::{
        message::{cli::CliRecv, tab_manager::TabManagerSend},
        prelude::*,
    };
    use lifeline::assert_completes;
    use tab_api::tab::TabId;

    #[tokio::test]
    async fn retask() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = listener_bus.tx::<TabManagerSend>()?;
        let mut rx = cli_bus.rx::<CliRecv>()?;

        tx.send(TabManagerSend::TabTerminated(TabId(0))).await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(CliRecv::TabStopped(TabId(0)), msg.unwrap());
        });

        Ok(())
    }
}

#[cfg(test)]
mod tabs_state_tests {
    use crate::{prelude::*, state::tab::TabsState};
    use lifeline::assert_completes;
    use std::collections::HashMap;

    #[tokio::test]
    async fn forward_state() -> anyhow::Result<()> {
        let cli_bus = CliBus::default();
        let listener_bus = ListenerBus::default();

        let _carrier = cli_bus.carry_from(&listener_bus)?;

        let mut tx = listener_bus.tx::<TabsState>()?;
        let mut rx = cli_bus.rx::<TabsState>()?;

        tx.send(TabsState {
            tabs: HashMap::new(),
        })
        .await?;

        assert_completes!(async move {
            let msg = rx.recv().await;
            assert!(msg.is_some());
            assert_eq!(
                TabsState {
                    tabs: HashMap::new()
                },
                msg.unwrap()
            );
        });

        Ok(())
    }
}
