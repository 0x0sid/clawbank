//! x402 payment interception and legitimacy screening.
//!
//! When an agent encounters an HTTP 402 (Payment Required) response,
//! this module intercepts the payment details and classifies risk
//! before releasing the payment signature. Suspicious payments are
//! flagged on the dashboard for human review.

use crate::types::{
    AppError, CreditLine, PolicyConfig, X402PaymentRequest, X402RiskLevel, X402Verdict,
};
use tracing::{info, warn};

/// Classify the risk level of an x402 payment request.
///
/// Rules:
/// - **High**: recipient is blocklisted, amount exceeds policy cap, or no credit line
/// - **Medium**: first-time recipient (not in allowlist), or amount > 50% of remaining budget
/// - **Low**: known recipient, amount within policy, matches strategy
pub fn classify_risk(
    payment: &X402PaymentRequest,
    credit_line: Option<&CreditLine>,
    policy: &PolicyConfig,
) -> (X402RiskLevel, String) {
    let recipient_lower = payment.recipient.to_lowercase();

    // Check 1: blocklisted recipient → HIGH
    if policy
        .x402_blocked_recipients
        .iter()
        .any(|b| b.to_lowercase() == recipient_lower)
    {
        return (
            X402RiskLevel::High,
            format!("Recipient {} is blocklisted", payment.recipient),
        );
    }

    // Check 2: amount exceeds x402 policy cap → HIGH
    if payment.amount_usd > policy.x402_max_payment_usd {
        return (
            X402RiskLevel::High,
            format!(
                "Amount ${:.2} exceeds x402 cap ${:.2}",
                payment.amount_usd, policy.x402_max_payment_usd
            ),
        );
    }

    // Check 3: no active credit line → HIGH
    let line = match credit_line {
        Some(l) => l,
        None => {
            return (
                X402RiskLevel::High,
                "No active credit line for agent".to_string(),
            );
        }
    };

    // Check 4: amount exceeds remaining budget → HIGH
    if payment.amount_usd > line.remaining_usd {
        return (
            X402RiskLevel::High,
            format!(
                "Amount ${:.2} exceeds remaining budget ${:.2}",
                payment.amount_usd, line.remaining_usd
            ),
        );
    }

    // Check 5: first-time recipient (not in allowlist) → MEDIUM
    let is_known = policy
        .x402_allowed_recipients
        .iter()
        .any(|a| a.to_lowercase() == recipient_lower);

    if !is_known {
        return (
            X402RiskLevel::Medium,
            format!(
                "First-time recipient {} — requires human review",
                payment.recipient
            ),
        );
    }

    // Check 6: large portion of remaining budget → MEDIUM
    if payment.amount_usd > line.remaining_usd * 0.5 {
        return (
            X402RiskLevel::Medium,
            format!(
                "Amount ${:.2} is >{:.0}% of remaining budget ${:.2}",
                payment.amount_usd, 50.0, line.remaining_usd
            ),
        );
    }

    // All checks passed → LOW
    (
        X402RiskLevel::Low,
        format!(
            "Known recipient, ${:.2} within budget — auto-approved",
            payment.amount_usd
        ),
    )
}

/// Intercept and evaluate an x402 payment request.
///
/// Returns a verdict that determines whether the payment should be:
/// - Auto-approved (Low risk)
/// - Flagged for human review (Medium risk)
/// - Auto-blocked (High risk)
pub async fn intercept_x402(
    payment: &X402PaymentRequest,
    credit_line: Option<&CreditLine>,
    policy: &PolicyConfig,
) -> Result<X402Verdict, AppError> {
    let (risk_level, reason) = classify_risk(payment, credit_line, policy);

    match risk_level {
        X402RiskLevel::Low => {
            info!(
                payment_id = %payment.id,
                agent_id = %payment.agent_id,
                recipient = %payment.recipient,
                amount_usd = payment.amount_usd,
                "x402 payment auto-approved (low risk)"
            );
            Ok(X402Verdict {
                payment_id: payment.id,
                approved: true,
                risk_level,
                reason,
                needs_human_review: false,
            })
        }
        X402RiskLevel::Medium => {
            warn!(
                payment_id = %payment.id,
                agent_id = %payment.agent_id,
                recipient = %payment.recipient,
                amount_usd = payment.amount_usd,
                reason = %reason,
                "x402 payment flagged for human review"
            );
            Ok(X402Verdict {
                payment_id: payment.id,
                approved: false,
                risk_level,
                reason,
                needs_human_review: true,
            })
        }
        X402RiskLevel::High => {
            warn!(
                payment_id = %payment.id,
                agent_id = %payment.agent_id,
                recipient = %payment.recipient,
                amount_usd = payment.amount_usd,
                reason = %reason,
                "x402 payment auto-blocked (high risk)"
            );
            Ok(X402Verdict {
                payment_id: payment.id,
                approved: false,
                risk_level,
                reason,
                needs_human_review: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ApprovedConditions, CreditStatus};
    use chrono::Utc;
    use uuid::Uuid;

    fn test_policy() -> PolicyConfig {
        PolicyConfig {
            x402_allowed_recipients: vec!["0xKnownGood".to_string()],
            x402_blocked_recipients: vec!["0xBadActor".to_string()],
            x402_max_payment_usd: 1.0,
            ..PolicyConfig::default()
        }
    }

    fn test_credit_line() -> CreditLine {
        CreditLine {
            id: Uuid::new_v4(),
            proposal_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            approved_usd: 5.0,
            spent_usd: 0.0,
            remaining_usd: 5.0,
            status: CreditStatus::Active,
            approved_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(24),
            conditions: ApprovedConditions {
                allowed_pairs: vec!["BTC-USDT".to_string()],
                max_single_trade_usd: 1.0,
                max_loss_usd: 3.0,
                window_end: Utc::now() + chrono::Duration::hours(24),
            },
            reputation_at_approval: 7.0,
        }
    }

    fn test_payment(recipient: &str, amount: f64) -> X402PaymentRequest {
        X402PaymentRequest {
            id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            recipient: recipient.to_string(),
            amount_usd: amount,
            currency: "USDC".to_string(),
            service_url: "https://api.example.com/data".to_string(),
            purpose: "Market data feed".to_string(),
            submitted_at: Utc::now(),
        }
    }

    #[test]
    fn test_known_recipient_low_risk() {
        let policy = test_policy();
        let line = test_credit_line();
        let payment = test_payment("0xKnownGood", 0.50);

        let (risk, _reason) = classify_risk(&payment, Some(&line), &policy);
        assert_eq!(risk, X402RiskLevel::Low);
    }

    #[test]
    fn test_blocklisted_recipient_high_risk() {
        let policy = test_policy();
        let line = test_credit_line();
        let payment = test_payment("0xBadActor", 0.50);

        let (risk, _reason) = classify_risk(&payment, Some(&line), &policy);
        assert_eq!(risk, X402RiskLevel::High);
    }

    #[test]
    fn test_exceeds_cap_high_risk() {
        let policy = test_policy();
        let line = test_credit_line();
        let payment = test_payment("0xKnownGood", 5.00);

        let (risk, _reason) = classify_risk(&payment, Some(&line), &policy);
        assert_eq!(risk, X402RiskLevel::High);
    }

    #[test]
    fn test_no_credit_line_high_risk() {
        let policy = test_policy();
        let payment = test_payment("0xKnownGood", 0.50);

        let (risk, _reason) = classify_risk(&payment, None, &policy);
        assert_eq!(risk, X402RiskLevel::High);
    }

    #[test]
    fn test_unknown_recipient_medium_risk() {
        let policy = test_policy();
        let line = test_credit_line();
        let payment = test_payment("0xNewRecipient", 0.50);

        let (risk, _reason) = classify_risk(&payment, Some(&line), &policy);
        assert_eq!(risk, X402RiskLevel::Medium);
    }

    #[test]
    fn test_large_budget_share_medium_risk() {
        let policy = test_policy();
        let mut line = test_credit_line();
        line.remaining_usd = 1.0; // only $1 left
        let payment = test_payment("0xKnownGood", 0.80); // 80% of budget

        let (risk, _reason) = classify_risk(&payment, Some(&line), &policy);
        assert_eq!(risk, X402RiskLevel::Medium);
    }

    #[tokio::test]
    async fn test_intercept_auto_approve() {
        let policy = test_policy();
        let line = test_credit_line();
        let payment = test_payment("0xKnownGood", 0.50);

        let verdict = intercept_x402(&payment, Some(&line), &policy)
            .await
            .expect("verdict");
        assert!(verdict.approved);
        assert_eq!(verdict.risk_level, X402RiskLevel::Low);
        assert!(!verdict.needs_human_review);
    }

    #[tokio::test]
    async fn test_intercept_flags_unknown() {
        let policy = test_policy();
        let line = test_credit_line();
        let payment = test_payment("0xNewRecipient", 0.50);

        let verdict = intercept_x402(&payment, Some(&line), &policy)
            .await
            .expect("verdict");
        assert!(!verdict.approved);
        assert_eq!(verdict.risk_level, X402RiskLevel::Medium);
        assert!(verdict.needs_human_review);
    }

    #[tokio::test]
    async fn test_intercept_auto_blocks() {
        let policy = test_policy();
        let line = test_credit_line();
        let payment = test_payment("0xBadActor", 0.50);

        let verdict = intercept_x402(&payment, Some(&line), &policy)
            .await
            .expect("verdict");
        assert!(!verdict.approved);
        assert_eq!(verdict.risk_level, X402RiskLevel::High);
        assert!(!verdict.needs_human_review);
    }
}
