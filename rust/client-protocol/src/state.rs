use std::fmt;

use time::OffsetDateTime;

use crate::ProtocolError;
use crate::auth::{CachedTokens, JwtClaims, RefreshOutcome, TokenResponse};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthState {
    NotConfigured,
    SignedOut,
    SignedIn,
    SignInNeeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshReason {
    MissingToken,
    MissingExpiry,
    Expiring,
    MalformedToken,
}

pub enum TokenDecision<'a> {
    NotConfigured,
    SignedOut,
    SignInNeeded,
    RefreshRequired(RefreshReason),
    Fresh(&'a str),
}

impl fmt::Debug for TokenDecision<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fresh(_) => f.write_str("Fresh(<redacted>)"),
            Self::RefreshRequired(reason) => write!(f, "RefreshRequired({reason:?})"),
            Self::NotConfigured => f.write_str("NotConfigured"),
            Self::SignedOut => f.write_str("SignedOut"),
            Self::SignInNeeded => f.write_str("SignInNeeded"),
        }
    }
}

#[derive(Debug)]
pub enum TransitionError<E> {
    Protocol(ProtocolError),
    Persistence(E),
}

pub struct AuthSession {
    configured: bool,
    state: AuthState,
    tokens: Option<CachedTokens>,
}

impl AuthSession {
    pub fn new(configured: bool, tokens: Option<CachedTokens>) -> Self {
        let has_refresh = tokens
            .as_ref()
            .and_then(|value| value.refresh_token.as_deref())
            .is_some_and(|value| !value.is_empty());
        let state = if !configured {
            AuthState::NotConfigured
        } else if has_refresh {
            AuthState::SignedIn
        } else {
            AuthState::SignedOut
        };
        Self {
            configured,
            state,
            tokens,
        }
    }

    pub fn state(&self) -> AuthState {
        self.state
    }

    pub(crate) fn email(&self) -> Option<String> {
        if self.state != AuthState::SignedIn {
            return None;
        }
        let tokens = self.tokens.as_ref()?;
        tokens
            .email
            .clone()
            .or_else(|| {
                tokens
                    .id_token
                    .as_ref()
                    .and_then(|token| JwtClaims::parse(token).ok())
                    .and_then(|claims| claims.email)
            })
            .filter(|email| !email.trim().is_empty())
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.tokens
            .as_ref()
            .and_then(|tokens| tokens.refresh_token.as_deref())
            .filter(|value| !value.is_empty())
    }

    pub fn token_decision(
        &self,
        now: OffsetDateTime,
        skew_seconds: i64,
    ) -> Result<TokenDecision<'_>, ProtocolError> {
        match self.state {
            AuthState::NotConfigured => return Ok(TokenDecision::NotConfigured),
            AuthState::SignedOut => return Ok(TokenDecision::SignedOut),
            AuthState::SignInNeeded => return Ok(TokenDecision::SignInNeeded),
            AuthState::SignedIn => {}
        }
        if skew_seconds < 0 {
            return Err(ProtocolError::InvalidField("refresh skew"));
        }
        let Some(token) = self
            .tokens
            .as_ref()
            .and_then(|value| value.id_token.as_deref())
            .filter(|value| !value.is_empty())
        else {
            return Ok(TokenDecision::RefreshRequired(RefreshReason::MissingToken));
        };
        let claims = match JwtClaims::parse(token) {
            Ok(claims) => claims,
            Err(_) => {
                return Ok(TokenDecision::RefreshRequired(
                    RefreshReason::MalformedToken,
                ));
            }
        };
        if claims.expires_at.is_none() {
            return Ok(TokenDecision::RefreshRequired(RefreshReason::MissingExpiry));
        }
        if claims.is_expiring(now, skew_seconds)? {
            Ok(TokenDecision::RefreshRequired(RefreshReason::Expiring))
        } else {
            Ok(TokenDecision::Fresh(token))
        }
    }

    fn require_sign_in(&mut self) {
        self.state = if self.configured {
            AuthState::SignInNeeded
        } else {
            AuthState::NotConfigured
        };
        if let Some(tokens) = &mut self.tokens {
            tokens.id_token = None;
            tokens.id_token_expires_at = None;
        }
    }

    pub fn accept_sign_in<E>(
        &mut self,
        response: TokenResponse,
        persist: impl FnOnce(&CachedTokens) -> Result<(), E>,
    ) -> Result<(), TransitionError<E>> {
        if !self.configured {
            return Err(TransitionError::Protocol(ProtocolError::InvalidField(
                "OAuth configuration",
            )));
        }
        if response.classify_refresh() != RefreshOutcome::Success
            || !response
                .refresh_token
                .as_deref()
                .is_some_and(|value| !value.is_empty())
        {
            self.require_sign_in();
            return Err(TransitionError::Protocol(ProtocolError::InvalidField(
                "sign-in token response",
            )));
        }
        let token = response
            .id_token
            .as_deref()
            .ok_or_else(|| TransitionError::Protocol(ProtocolError::InvalidField("ID token")))?;
        let claims = JwtClaims::parse(token).map_err(|error| {
            self.require_sign_in();
            TransitionError::Protocol(error)
        })?;
        let tokens = CachedTokens {
            id_token: response.id_token,
            refresh_token: response.refresh_token,
            id_token_expires_at: claims.expires_at,
            email: claims.email,
        };
        persist(&tokens).map_err(TransitionError::Persistence)?;
        self.tokens = Some(tokens);
        self.state = AuthState::SignedIn;
        Ok(())
    }

    pub fn apply_refresh<E>(
        &mut self,
        response: TokenResponse,
        persist: impl FnOnce(&CachedTokens) -> Result<(), E>,
    ) -> Result<RefreshOutcome, TransitionError<E>> {
        if !self.configured
            || self
                .tokens
                .as_ref()
                .and_then(|value| value.refresh_token.as_deref())
                .is_none_or(str::is_empty)
        {
            return Ok(RefreshOutcome::RefreshRevoked);
        }
        let outcome = response.classify_refresh();
        match outcome {
            RefreshOutcome::Transient => return Ok(outcome),
            RefreshOutcome::RefreshRevoked | RefreshOutcome::NoIdToken => {
                self.require_sign_in();
                if let Some(tokens) = &self.tokens {
                    persist(tokens).map_err(TransitionError::Persistence)?;
                }
                return Ok(outcome);
            }
            RefreshOutcome::Success => {}
        }
        let token = response
            .id_token
            .as_deref()
            .ok_or_else(|| TransitionError::Protocol(ProtocolError::InvalidField("ID token")))?;
        let claims = JwtClaims::parse(token).map_err(|error| {
            self.require_sign_in();
            TransitionError::Protocol(error)
        })?;
        let mut tokens = self.tokens.clone().ok_or_else(|| {
            TransitionError::Protocol(ProtocolError::InvalidField("refresh cache"))
        })?;
        tokens.id_token = response.id_token;
        tokens.id_token_expires_at = claims.expires_at;
        tokens.email = claims.email.or(tokens.email);
        if let Some(rotated) = response.refresh_token.filter(|value| !value.is_empty()) {
            tokens.refresh_token = Some(rotated);
        }
        // Do not publish the new in-memory session until its durable cache is acknowledged.
        persist(&tokens).map_err(TransitionError::Persistence)?;
        self.tokens = Some(tokens);
        self.state = AuthState::SignedIn;
        Ok(outcome)
    }

    pub fn sign_out<E>(
        &mut self,
        clear: impl FnOnce() -> Result<(), E>,
    ) -> Result<(), TransitionError<E>> {
        let deleted = clear();
        self.tokens = None;
        self.state = if self.configured {
            AuthState::SignedOut
        } else {
            AuthState::NotConfigured
        };
        deleted.map_err(TransitionError::Persistence)
    }
}
