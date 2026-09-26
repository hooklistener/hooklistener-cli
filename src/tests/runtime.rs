//! JSON worker supervision and terminal setup cleanup.

use super::*;

#[tokio::test]
async fn json_worker_is_cancelled_and_awaited_on_injected_shutdown() {
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    struct NotifyDrop(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for NotifyDrop {
        fn drop(&mut self) {
            let _ = self.0.take().expect("sender").send(());
        }
    }
    let worker = tokio::spawn(async move {
        let _guard = NotifyDrop(Some(dropped_tx));
        std::future::pending::<()>().await;
    });
    tokio::task::yield_now().await;
    let result = supervise_json_worker(
        worker,
        std::future::pending::<()>(),
        std::future::ready(Ok(())),
    )
    .await
    .unwrap();
    assert_eq!(result, WorkerCompletion::Shutdown);
    dropped_rx.await.expect("worker drop notification");
}

#[test]
fn terminal_initialization_cleanup_state_can_be_disarmed_without_terminal_io() {
    let mut cleanup = TerminalInitCleanup::raw_mode_enabled();
    assert!(cleanup.raw_mode);
    cleanup.alternate_screen = true;
    cleanup.disarm();
    assert_eq!(cleanup, TerminalInitCleanup::default());
}
