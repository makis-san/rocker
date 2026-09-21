# Rocker glossary

## Kubernetes engine

A Kubernetes context available to Rocker from a local kubeconfig. It identifies
a cluster and user, but does not expose the kubeconfig's credentials to an
extension.

## Kubernetes workload

A resource running in a Kubernetes engine. The first visible workload type is
a Pod, identified by its Kubernetes connection, namespace, and name.
