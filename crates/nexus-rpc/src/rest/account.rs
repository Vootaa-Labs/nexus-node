// Copyright (c) The Nexus-Node Contributors
// SPDX-License-Identifier: Apache-2.0

//! Account query endpoints.
//!
//! `GET /v2/account/{addr}/balance` — aggregated balance for an account.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};

use super::AppState;
use crate::dto::{AccountBalanceDto, TokenBalanceDto};
use crate::error::RpcResult;
use nexus_primitives::TokenId;

/// Build the account router fragment.
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/v2/account/:addr/balance", get(account_balance))
}

/// `GET /v2/account/{addr}/balance`
///
/// Returns the native token balance for the given account address (hex-encoded).
async fn account_balance(
    State(state): State<Arc<AppState>>,
    Path(addr_hex): Path<String>,
) -> RpcResult<Json<AccountBalanceDto>> {
    let address = super::parse_address(&addr_hex)?;
    let amount = state.query.account_balance(&address, &TokenId::Native)?;
    Ok(Json(AccountBalanceDto {
        address,
        balances: vec![TokenBalanceDto {
            token: TokenId::Native,
            amount,
        }],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rest::test_helpers::{state_with_backend, MockQueryBackend};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use nexus_primitives::{AccountAddress, Amount};
    use tower::ServiceExt;

    #[tokio::test]
    async fn balance_returns_200_with_known_account() {
        let addr = AccountAddress([0xAA; 32]);
        let backend =
            MockQueryBackend::new().with_balance(addr, TokenId::Native, Amount(1_000_000));
        let state = state_with_backend(backend);
        let app = router().with_state(state);

        let addr_hex = hex::encode(addr.0);
        let req = Request::builder()
            .uri(format!("/v2/account/{addr_hex}/balance"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let dto: AccountBalanceDto = serde_json::from_slice(&body).unwrap();
        assert_eq!(dto.address, addr);
        assert_eq!(dto.balances.len(), 1);
        assert_eq!(dto.balances[0].amount, Amount(1_000_000));
    }

    #[tokio::test]
    async fn balance_returns_404_for_unknown_account() {
        let state = state_with_backend(MockQueryBackend::new());
        let app = router().with_state(state);

        let addr_hex = hex::encode([0xBB; 32]);
        let req = Request::builder()
            .uri(format!("/v2/account/{addr_hex}/balance"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn balance_returns_400_for_invalid_hex() {
        let state = state_with_backend(MockQueryBackend::new());
        let app = router().with_state(state);

        let req = Request::builder()
            .uri("/v2/account/not-hex/balance")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn balance_returns_400_for_wrong_length() {
        let state = state_with_backend(MockQueryBackend::new());
        let app = router().with_state(state);

        let short_hex = hex::encode([0xCC; 16]); // 16 bytes instead of 32
        let req = Request::builder()
            .uri(format!("/v2/account/{short_hex}/balance"))
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parse_address_valid() {
        let hex_str = hex::encode([0xDD; 32]);
        let result = crate::rest::parse_address(&hex_str);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), AccountAddress([0xDD; 32]));
    }

    #[test]
    fn parse_address_invalid_hex() {
        let result = crate::rest::parse_address("zzzz");
        assert!(result.is_err());
    }

    #[test]
    fn parse_address_wrong_length() {
        let hex_str = hex::encode([0xEE; 16]);
        let result = crate::rest::parse_address(&hex_str);
        assert!(result.is_err());
    }
}
