use serde::{Deserialize, Serialize};

/// Kind of interrupt injected into the agent loop.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum InterruptKind {
    /// Non-blocking soft interrupt (injected at safe points)
    Soft,
    /// Swarm alert from coordinator
    Alert,
}

/// A concrete interrupt that was injected into a turn.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InjectedInterrupt {
    /// The interrupt message content
    pub content: String,
    /// Whether this interrupt is urgent
    pub urgent: bool,
    /// Interrupt category
    pub kind: InterruptKind,
}

use std::collections::VecDeque;

/// Manages the lifecycle of soft interrupts and graceful shutdown signals.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct InterruptSnapshot {
    #[serde(default)]
    pub pending_interrupts: Vec<InjectedInterrupt>,
    #[serde(default)]
    pub pending_alerts: Vec<String>,
    pub graceful_shutdown_requested: bool,
}

#[derive(Debug, Clone, Default)]
pub struct InterruptManager {
    /// Queue of pending soft interrupts
    soft_interrupts: VecDeque<InjectedInterrupt>,
    /// Swarm alert messages to inject
    pending_alerts: Vec<String>,
    /// Whether graceful shutdown has been requested
    graceful_shutdown_requested: bool,
}

impl InterruptManager {
    pub fn queue_soft_interrupt(&mut self, content: impl Into<String>, urgent: bool) {
        self.soft_interrupts.push_back(InjectedInterrupt {
            content: content.into(),
            urgent,
            kind: InterruptKind::Soft,
        });
    }

    pub fn queue_alert(&mut self, alert: impl Into<String>) {
        self.pending_alerts.push(alert.into());
    }

    pub fn request_graceful_shutdown(&mut self) {
        self.graceful_shutdown_requested = true;
    }

    /// Clear the graceful-shutdown flag.
    ///
    /// Called when a new user-initiated turn begins — a fresh user message
    /// means the user wants to continue working, so a shutdown requested to
    /// cancel a *previous* turn must not carry over and immediately cancel
    /// the new one.
    pub fn clear_graceful_shutdown(&mut self) {
        self.graceful_shutdown_requested = false;
    }

    pub fn is_graceful_shutdown_requested(&self) -> bool {
        self.graceful_shutdown_requested
    }

    pub fn snapshot(&self) -> InterruptSnapshot {
        InterruptSnapshot {
            pending_interrupts: self.soft_interrupts.iter().cloned().collect(),
            pending_alerts: self.pending_alerts.clone(),
            graceful_shutdown_requested: self.graceful_shutdown_requested,
        }
    }

    pub fn restore(&mut self, snapshot: InterruptSnapshot) {
        self.soft_interrupts = snapshot.pending_interrupts.into();
        self.pending_alerts = snapshot.pending_alerts;
        self.graceful_shutdown_requested = snapshot.graceful_shutdown_requested;
    }

    pub fn take_pending_interrupts(&mut self) -> Vec<InjectedInterrupt> {
        let mut events = self.soft_interrupts.drain(..).collect::<Vec<_>>();
        events.extend(
            self.pending_alerts
                .drain(..)
                .map(|content| InjectedInterrupt {
                    content,
                    urgent: false,
                    kind: InterruptKind::Alert,
                }),
        );
        events
    }
}
