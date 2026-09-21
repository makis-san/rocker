//! Kubernetes domain models shared between the engine and UI.

use serde::{Deserialize, Serialize};

/// A workload addressed within one Kubernetes connection and namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KubernetesWorkloadRef {
    /// Kubeconfig context used to reach the workload.
    pub connection: String,
    /// Namespace containing the workload.
    pub namespace: String,
    /// Kubernetes controller kind, such as `Deployment` or `StatefulSet`.
    pub kind: String,
    /// Controller name.
    pub name: String,
}

/// The immediate controller recorded on a Pod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubernetesOwner {
    /// Kubernetes resource kind, such as `ReplicaSet` or `StatefulSet`.
    pub kind: String,
    /// Resource name in the Pod's namespace.
    pub name: String,
}

/// A read-only summary of one Kubernetes Pod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubernetesPod {
    /// Kubernetes context through which Rocker discovered the Pod.
    pub connection: String,
    /// Namespace containing the Pod.
    pub namespace: String,
    /// Kubernetes object name.
    pub name: String,
    /// Immediate workload owner, when this is a managed Pod.
    pub owner: Option<KubernetesOwner>,
    /// Lifecycle phase reported by the API, such as `Running` or `Pending`.
    pub phase: String,
    /// Ready-container count, formatted as `ready/total`.
    pub ready: String,
}
