#!/usr/bin/env bash
set -uo pipefail

ARTIFACT_DIR="${ARTIFACT_DIR:-artifacts/gke-integration}"
OPERATOR_NAMESPACE="${OPERATOR_NAMESPACE:-ndn-operator}"
NETWORK_NAMESPACE="${NETWORK_NAMESPACE:-mynetwork}"
WORKLOAD_NAMESPACE="${WORKLOAD_NAMESPACE:-ndn-workloads}"

mkdir -p "${ARTIFACT_DIR}"

run() {
  local name="$1"
  shift
  echo "Collecting ${name}"
  "$@" >"${ARTIFACT_DIR}/${name}.txt" 2>&1 || true
}

run cluster-info kubectl cluster-info
run nodes-wide kubectl get nodes -o wide
run all-namespaces kubectl get all -A -o wide
run events kubectl get events -A --sort-by=.lastTimestamp
run crds kubectl get crd
run networks kubectl get networks -A -o yaml
run routers kubectl get routers -A -o yaml
run certificates kubectl get certificates -A -o yaml
run external-certificates kubectl get externalcertificates -A -o yaml
run neighbors kubectl get neighbors -A -o yaml
run operator-pods kubectl -n "${OPERATOR_NAMESPACE}" describe pods
run network-pods kubectl -n "${NETWORK_NAMESPACE}" describe pods
run workload-pods kubectl -n "${WORKLOAD_NAMESPACE}" describe pods

kubectl -n "${OPERATOR_NAMESPACE}" logs deployment/ndn-controller --all-containers \
  >"${ARTIFACT_DIR}/ndn-controller.log" 2>&1 || true
kubectl -n "${OPERATOR_NAMESPACE}" logs deployment/ndn-injector --all-containers \
  >"${ARTIFACT_DIR}/ndn-injector.log" 2>&1 || true

for pod in $(kubectl -n "${NETWORK_NAMESPACE}" get pods -o name 2>/dev/null); do
  safe_name="${pod#pod/}"
  kubectl -n "${NETWORK_NAMESPACE}" logs "${pod}" --all-containers \
    >"${ARTIFACT_DIR}/network-${safe_name}.log" 2>&1 || true
done

for pod in $(kubectl -n "${WORKLOAD_NAMESPACE}" get pods -o name 2>/dev/null); do
  safe_name="${pod#pod/}"
  kubectl -n "${WORKLOAD_NAMESPACE}" logs "${pod}" --all-containers \
    >"${ARTIFACT_DIR}/workload-${safe_name}.log" 2>&1 || true
done
