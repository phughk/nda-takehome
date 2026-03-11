use anyhow::Result as AnyResult;
use nda_takehome::service::config::ServiceConfig;
use nda_takehome::service::{Service, ServiceMessage};
use nda_takehome::CsvReader;
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_end_to_end_basic_csv() -> AnyResult<()> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/basic.csv");
    let mut reader = CsvReader::new(path).await?;
    let mut messages = Vec::new();
    while let Some(msg) = reader.next().await? {
        messages.push(msg);
    }

    let service = Service::new(ServiceConfig::default());
    let ctx = CancellationToken::new();
    let (svc_sx, svc_rx) = tokio::sync::mpsc::unbounded_channel();
    let (tx_sx, mut tx_rx) = tokio::sync::mpsc::unbounded_channel();

    let svc_clone = service.clone();
    let svc_handle = tokio::spawn(async move {
        svc_clone.server_forever(ctx.clone(), svc_rx).await
    });

    for msg in messages {
        svc_sx.send(ServiceMessage::Incoming(Box::new(msg), tx_sx.clone()))?;
    }

    let (signal_sx, signal_rx) = tokio::sync::oneshot::channel();
    svc_sx.send(ServiceMessage::TransactionBatchCompletion(signal_sx))?;
    signal_rx.await?;

    // Collect transaction results
    drop(tx_sx);
    let mut results = vec![];
    while let Some(r) = tx_rx.recv().await {
        results.push(r);
    }

    // Write snapshot and parse
    let mut output = Vec::new();
    service.write_snapshot(&mut output).await?;
    let csv = String::from_utf8(output)?;

    drop(svc_sx);
    let _ = svc_handle.await;

    // Parse the output CSV into a map keyed by client_id
    let mut rdr = csv::Reader::from_reader(csv.as_bytes());
    let mut accounts: HashMap<String, HashMap<String, String>> = HashMap::new();
    for record in rdr.deserialize() {
        let row: HashMap<String, String> = record?;
        let client = row.get("client").unwrap().clone();
        accounts.insert(client, row);
    }

    // Client 1: deposit 100, withdraw 50, deposit 25.5, dispute tx=1 (100), resolve tx=1 (100)
    // available = 100 - 50 + 25.5 - 100 + 100 = 75.5, held = 0, total = 75.5
    let c1 = &accounts["1"];
    assert_eq!(c1["available"], "75.5000");
    assert_eq!(c1["held"], "0.0000");
    assert_eq!(c1["total"], "75.5000");
    assert_eq!(c1["locked"], "false");

    // Client 2: deposit 200
    let c2 = &accounts["2"];
    assert_eq!(c2["available"], "200.0000");
    assert_eq!(c2["held"], "0.0000");
    assert_eq!(c2["total"], "200.0000");
    assert_eq!(c2["locked"], "false");

    // Client 3: deposit 500, dispute 300, chargeback 300 → locked
    // available = 500 - 300 + 0 = 200, held = 0 (300 removed), total = 500 - 300 = 200
    let c3 = &accounts["3"];
    assert_eq!(c3["available"], "200.0000");
    assert_eq!(c3["held"], "0.0000");
    assert_eq!(c3["total"], "200.0000");
    assert_eq!(c3["locked"], "true");

    Ok(())
}
