#!/usr/bin/env bash
set -uo pipefail

ARTIFACT_DIR="${ARTIFACT_DIR:-artifacts/gke-integration}"
KUBECONFIG_PATH="${KUBECONFIG_PATH:-${KUBECONFIG:-}}"
OPERATOR_NAMESPACE="${OPERATOR_NAMESPACE:-ndn-operator}"
NETWORK_NAMESPACE="${NETWORK_NAMESPACE:-mynetwork}"
WORKLOAD_NAMESPACE="${WORKLOAD_NAMESPACE:-ndn-workloads}"

mkdir -p "${ARTIFACT_DIR}"

kc() {
  if [[ -n "${KUBECONFIG_PATH}" ]]; then
    kubectl --kubeconfig "${KUBECONFIG_PATH}" "$@"
  else
    kubectl "$@"
  fi
}

run() {
  local name="$1"
  shift
  echo "Collecting ${name}"
  "$@" >"${ARTIFACT_DIR}/${name}.txt" 2>&1 || true
}

run cluster-info kc cluster-info
run nodes-wide kc get nodes -o wide
run all-namespaces kc get all -A -o wide
run events kc get events -A --sort-by=.lastTimestamp
run crds kc get crd
run networks kc get networks -A -o yaml
run routers kc get routers -A -o yaml
run certificates kc get certificates -A -o yaml
run external-certificates kc get externalcertificates -A -o yaml
run neighbors kc get neighbors -A -o yaml
run operator-pods kc -n "${OPERATOR_NAMESPACE}" describe pods
run network-pods kc -n "${NETWORK_NAMESPACE}" describe pods
run workload-pods kc -n "${WORKLOAD_NAMESPACE}" describe pods

kc -n "${OPERATOR_NAMESPACE}" logs deployment/ndn-controller --all-containers \
  >"${ARTIFACT_DIR}/ndn-controller.log" 2>&1 || true
kc -n "${OPERATOR_NAMESPACE}" logs deployment/ndn-injector --all-containers \
  >"${ARTIFACT_DIR}/ndn-injector.log" 2>&1 || true

for pod in $(kc -n "${NETWORK_NAMESPACE}" get pods -o name 2>/dev/null); do
  safe_name="${pod#pod/}"
  kc -n "${NETWORK_NAMESPACE}" logs "${pod}" --all-containers \
    >"${ARTIFACT_DIR}/network-${safe_name}.log" 2>&1 || true
done

for pod in $(kc -n "${WORKLOAD_NAMESPACE}" get pods -o name 2>/dev/null); do
  safe_name="${pod#pod/}"
  kc -n "${WORKLOAD_NAMESPACE}" logs "${pod}" --all-containers \
    >"${ARTIFACT_DIR}/workload-${safe_name}.log" 2>&1 || true
done
