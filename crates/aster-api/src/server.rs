//! Stop accepting on a signal, but bound draining of outstanding requests.

use std::{future::IntoFuture, io, time::Duration};

use axum::Router;
use tokio::{net::TcpListener, sync::watch};

pub async fn serve_with_shutdown(
    listener: TcpListener,
    app: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
    drain_timeout: Duration,
) -> io::Result<()> {
    let (stopping, mut stopped) = watch::channel(false);
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            let _ = stopping.send(true);
        })
        .into_future();
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => result,
        _ = stopped.changed() => {
            match tokio::time::timeout(drain_timeout, &mut server).await {
                Ok(result) => result,
                Err(_) => {
                    // The composition root closes its pool and exits the Tokio
                    // runtime next, which cancels any remaining handler tasks.
                    tracing::warn!("HTTP drain deadline reached; finishing shutdown");
                    Ok(())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::routing::get;
    use tokio::{
        io::AsyncWriteExt,
        net::TcpStream,
        sync::{Notify, oneshot},
    };

    use super::*;

    #[tokio::test]
    async fn idle_server_shuts_down_cleanly() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            serve_with_shutdown(listener, Router::new(), async {}, Duration::from_secs(1)),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn a_handler_that_never_finishes_cannot_block_shutdown() {
        let entered = Arc::new(Notify::new());
        let handler_entered = entered.clone();
        let app = Router::new().route(
            "/",
            get(move || {
                let entered = handler_entered.clone();
                async move {
                    entered.notify_one();
                    std::future::pending::<String>().await
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(serve_with_shutdown(
            listener,
            app,
            async move {
                let _ = stopped.await;
            },
            Duration::from_millis(10),
        ));
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), entered.notified())
            .await
            .unwrap();
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
