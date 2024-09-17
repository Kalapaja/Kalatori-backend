use crate::{
    definitions::api_v2::*,
    error::{Error, ForceWithdrawalError, OrderError, ServerError},
    state::State,
};
use axum::{
    extract::{self, rejection::RawPathParamsRejection, MatchedPath, Query, RawPathParams},
    http::{header, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing, Json, Router,
};
use axum_macros::debug_handler;
use serde::{Serialize, Deserialize, Serializer};
use std::{borrow::Cow, collections::HashMap, future::Future, net::SocketAddr};

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub const MODULE: &str = module_path!();

pub async fn new(
    shutdown_notification: CancellationToken,
    host: SocketAddr,
    state: State,
) -> Result<impl Future<Output = Result<Cow<'static, str>, Error>>, ServerError> {
    let v2: Router<State> = Router::new()
        .route("/order/:order_id", routing::post(order))
        .route(
            "/order/:order_id/forceWithdrawal",
            routing::post(force_withdrawal),
        )
        .route("/status", routing::get(status))
        .route("/health", routing::get(health))
        .route("/audit", routing::get(audit))
        .route("/order/:order_id/investigate", routing::post(investigate));
    let app = Router::new()
        .route(
            "/public/v2/payment/:paymentAccount",
            routing::post(public_payment_account),
        )
        .nest("/v2", v2)
        .with_state(state);

    let listener = TcpListener::bind(host)
        .await
        .map_err(|_| ServerError::TcpListenerBind(host))?;

    Ok(async {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_notification.cancelled_owned())
            .await
            .map_err(|_| ServerError::ThreadError)?;

        Ok("The server module is shut down.".into())
    })
}

#[derive(Debug, Serialize)]
struct InvalidParameter {
    parameter: String,
    message: String,
}

async fn process_order(
    state: State,
    matched_path: &MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
    payload: OrderQuery,
) -> Result<OrderResponse, OrderError> {
    const ORDER_ID: &str = "order_id";

    let path_parameters =
        path_result.map_err(|_| OrderError::InvalidParameter(matched_path.as_str().to_owned()))?;
    let order = path_parameters
        .iter()
        .find_map(|(key, value)| (key == ORDER_ID).then_some(value))
        .ok_or_else(|| OrderError::MissingParameter(ORDER_ID.into()))?
        .to_owned();

    let currency = payload.currency;
    let amount = payload.amount;
    let callback = payload.callback;

    if amount < 0.07 {
        return Err(OrderError::LessThanExistentialDeposit(0.07));
    }

    state
        .create_order(OrderQuery {
            order,
            amount,
            callback,
            currency,
        })
        .await
        .map_err(|_| OrderError::InternalError)
}



#[debug_handler]
async fn order(
    extract::State(state): extract::State<State>,
    matched_path: MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
    extract::Path(order_id): extract::Path<String>,
    Json(mut payload): Json<HashMap<String, serde_json::Value>>,
) -> Response {
    // Manually constructing OrderQuery because need to mix 2 path and payload
    let currency = payload
        .remove("currency")
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "Unknown".to_string());

    let amount = payload
        .remove("amount")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let callback = payload
        .remove("callback")
        .and_then(|v| v.as_str().map(String::from));

    let order_query = OrderQuery {
        order: order_id,
        currency,
        amount,
        callback,
    };

    match process_order(state, &matched_path, path_result, order_query).await {
        Ok(order) => match order {
            OrderResponse::NewOrder(order_status) => (StatusCode::CREATED, Json(order_status)).into_response(),
            OrderResponse::FoundOrder(order_status) => (StatusCode::OK, Json(order_status)).into_response(),
            OrderResponse::ModifiedOrder(order_status) => (StatusCode::OK, Json(order_status)).into_response(),
            OrderResponse::CollidedOrder(order_status) => (StatusCode::CONFLICT, Json(order_status)).into_response(),
            OrderResponse::NotFound => (StatusCode::NOT_FOUND, "").into_response(),
        },
        Err(error) => match error {
            OrderError::LessThanExistentialDeposit(existential_deposit) => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter: "amount".into(),
                    message: format!("provided amount is less than the currency's existential deposit ({existential_deposit})"),
                }]),
            )
                .into_response(),
            OrderError::UnknownCurrency => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter: "currency".into(),
                    message: "provided currency isn't supported".into(),
                }]),
            )
                .into_response(),
            OrderError::MissingParameter(parameter) => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter,
                    message: "parameter wasn't found".into(),
                }]),
            )
                .into_response(),
            OrderError::InvalidParameter(parameter) => (
                StatusCode::BAD_REQUEST,
                Json([InvalidParameter {
                    parameter,
                    message: "parameter's format is invalid".into(),
                }]),
            )
                .into_response(),
            OrderError::InternalError => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
    }
}

async fn process_force_withdrawal(
    state: State,
    matched_path: &MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
) -> Result<OrderStatus, ForceWithdrawalError> {
    const ORDER_ID: &str = "order_id";

    let path_parameters = path_result
        .map_err(|_| ForceWithdrawalError::InvalidParameter(matched_path.as_str().to_owned()))?;
    let order = path_parameters
        .iter()
        .find_map(|(key, value)| (key == ORDER_ID).then_some(value))
        .ok_or_else(|| ForceWithdrawalError::MissingParameter(ORDER_ID.into()))?
        .to_owned();
    state
        .force_withdrawal(order)
        .await
        .map_err(|e| ForceWithdrawalError::WithdrawalError(e.into()))
}

#[debug_handler]
async fn force_withdrawal(
    extract::State(state): extract::State<State>,
    matched_path: MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
) -> Response {
    match process_force_withdrawal(state, &matched_path, path_result).await {
        Ok(a) => (StatusCode::CREATED, Json(a)).into_response(),
        Err(ForceWithdrawalError::WithdrawalError(a)) => {
            (StatusCode::BAD_REQUEST, Json(a)).into_response()
        }
        Err(ForceWithdrawalError::MissingParameter(parameter)) => (
            StatusCode::BAD_REQUEST,
            Json([InvalidParameter {
                parameter,
                message: "parameter wasn't found".into(),
            }]),
        )
            .into_response(),
        Err(ForceWithdrawalError::InvalidParameter(parameter)) => (
            StatusCode::BAD_REQUEST,
            Json([InvalidParameter {
                parameter,
                message: "parameter's format is invalid".into(),
            }]),
        )
            .into_response(),
    }
}

async fn status(
    extract::State(state): extract::State<State>,
) -> ([(HeaderName, &'static str); 1], Json<ServerStatus>) {
    match state.server_status().await {
        Ok(status) => ([(header::CACHE_CONTROL, "no-store")], status.into()),
        Err(_e) => panic!("db connection is down, state is lost"), //TODO tell this to client
    }
}

async fn health(
    extract::State(state): extract::State<State>,
) -> ([(HeaderName, &'static str); 1], Json<ServerStatus>) {
    todo!();
}

async fn audit(extract::State(state): extract::State<State>) -> Response {
    StatusCode::NOT_IMPLEMENTED.into_response()
}

#[debug_handler]
async fn investigate(
    extract::State(state): extract::State<State>,
    matched_path: MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
    query: Query<HashMap<String, String>>,
) -> Response {
    todo!()
}

#[debug_handler]
async fn public_payment_account(
    extract::State(state): extract::State<State>,
    matched_path: MatchedPath,
    path_result: Result<RawPathParams, RawPathParamsRejection>,
    query: Query<HashMap<String, String>>,
) -> Response {
    todo!()
}
