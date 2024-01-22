use crate::{
    database::{Database, Invoice, InvoiceStatus},
    Account,
};
use anyhow::{Context, Result};
use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use std::{future::Future, net::SocketAddr, sync::Arc};
use subxt::ext::sp_core::{hexdisplay::HexDisplay, DeriveJunction, Pair};
use tokio::{net::TcpListener, sync::watch::Receiver};

pub(crate) const MODULE: &str = module_path!();

#[derive(Serialize)]
#[serde(untagged)]
pub enum Response {
    Error(String),
    Success {
        pay_account: String,
        price: u128,
        recipient: String,
        order: String,
        wss: String,
        mul: u64,
        result: String,
        version: String,
    },
}

pub(crate) async fn new(
    mut shutdown_notification: Receiver<bool>,
    host: SocketAddr,
    database: Arc<Database>,
) -> Result<impl Future<Output = Result<&'static str>>> {
    let app = Router::new()
        .route(
            "/recipient/:recipient/order/:order/price/:price",
            get(handler),
        )
        .with_state(database);

    let listener = TcpListener::bind(host)
        .await
        .context("failed to bind the TCP listener")?;

    log::info!("The server is listening on {host:?}.");

    Ok(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                drop(shutdown_notification.changed().await);
            })
            .await
            .context("failed to fire up the server")?;

        Ok("The server module is shut down.")
    })
}

async fn handler(
    State(database): State<Arc<Database>>,
    Path((recipient, order, price)): Path<(String, String, u128)>,
) -> Json<Response> {
    let decoded_recip = hex::decode(&recipient[2..]).unwrap();
    let recipient_account = Account::try_from(decoded_recip.as_ref()).unwrap();
    let properties = database.properties().await;
    let order_encoded = DeriveJunction::hard(&order).unwrap_inner();
    let invoice_account: Account = database
        .pair()
        .derive(
            [
                DeriveJunction::Hard(<[u8; 32]>::from(recipient_account.clone())),
                DeriveJunction::Hard(order_encoded),
            ]
            .into_iter(),
            None,
        )
        .unwrap()
        .0
        .public()
        .into();

    if let Some(encoded_invoice) = database
        .read()
        .unwrap()
        .invoices()
        .unwrap()
        .get(&invoice_account)
        .unwrap()
    {
        let invoice = encoded_invoice.value();

        if let InvoiceStatus::Unpaid(saved_price) = invoice.status {
            if saved_price != price {
                return Response::Error(format!(
                    "The invoice was created with different price ({price})."
                ))
                .into();
            }
        }

        Response::Success {
            pay_account: format!("0x{}", HexDisplay::from(&invoice_account.as_ref())),
            price: match invoice.status {
                InvoiceStatus::Unpaid(uprice) => uprice,
                InvoiceStatus::Paid(uprice) => uprice,
            },
            wss: database.rpc().to_string(),
            mul: properties.decimals,
            recipient,
            order,
            result: match invoice.status {
                InvoiceStatus::Unpaid(_) => "waiting",
                InvoiceStatus::Paid(_) => "paid",
            }
            .into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
        .into()
    } else {
        let tx = database.write().unwrap();

        tx.invoices()
            .unwrap()
            .save(
                &invoice_account,
                &Invoice {
                    recipient: recipient_account,
                    order: order_encoded,
                    status: InvoiceStatus::Unpaid(price),
                },
            )
            .unwrap();

        tx.commit().unwrap();

        Response::Success {
            pay_account: format!("0x{}", HexDisplay::from(&invoice_account.as_ref())),
            price,
            wss: database.rpc().to_string(),
            mul: properties.decimals,
            recipient,
            order,
            version: env!("CARGO_PKG_VERSION").into(),
            result: "waiting".into(),
        }
        .into()
    }
}
