#!/usr/bin/env bash
set -euo pipefail

DEFAULT_ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ROOT_DIR="${REPO_UNDER_TEST_DIR:-${DEFAULT_ROOT_DIR}}"

: "${OPERATOR_IMAGE:?OPERATOR_IMAGE must be set, for example ghcr.io/owner/repo:pr-123}"

OPERATOR_NAMESPACE="${OPERATOR_NAMESPACE:-ndn-operator}"
NETWORK_NAMESPACE="${NETWORK_NAMESPACE:-mynetwork}"
WORKLOAD_NAMESPACE="${WORKLOAD_NAMESPACE:-ndn-workloads}"
NETWORK_NAME="${NETWORK_NAME:-test}"
ARM_TOLERATION='[{"key":"kubernetes.io/arch","operator":"Equal","value":"arm64","effect":"NoSchedule"}]'
ARM_POD_TOLERATION_PATCH="{\"spec\":{\"tolerations\":${ARM_TOLERATION}}}"
ARM_TEMPLATE_TOLERATION_PATCH="{\"spec\":{\"template\":{\"spec\":{\"tolerations\":${ARM_TOLERATION}}}}}"

if [[ "${OPERATOR_IMAGE}" != *:* ]]; then
  echo "OPERATOR_IMAGE must include a tag: ${OPERATOR_IMAGE}" >&2
  exit 1
fi

IMAGE_REPOSITORY="${OPERATOR_IMAGE%:*}"
IMAGE_TAG="${OPERATOR_IMAGE##*:}"

echo "Installing ndn-operator from ${OPERATOR_IMAGE}"
helm upgrade --install ndn-operator "${ROOT_DIR}/charts/ndn-operator" \
  --namespace "${OPERATOR_NAMESPACE}" \
  --create-namespace \
  --set "image.repository=${IMAGE_REPOSITORY}" \
  --set "image.tag=${IMAGE_TAG}" \
  --set "image.pullPolicy=Always" \
  --set-json "tolerations=${ARM_TOLERATION}" \
  --wait \
  --timeout 5m

kubectl -n "${OPERATOR_NAMESPACE}" rollout status deployment/ndn-controller --timeout=5m
kubectl -n "${OPERATOR_NAMESPACE}" rollout status deployment/ndn-injector --timeout=5m

kubectl create namespace "${NETWORK_NAMESPACE}" --dry-run=client -o yaml | kubectl apply -f -
kubectl create namespace "${WORKLOAD_NAMESPACE}" --dry-run=client -o yaml | kubectl apply -f -

echo "Applying secured NDN network example"
kubectl -n "${NETWORK_NAMESPACE}" apply -f "${ROOT_DIR}/examples/secure/self-signed-cert.yaml"
kubectl -n "${NETWORK_NAMESPACE}" apply -f "${ROOT_DIR}/examples/secure/network.yaml"
kubectl -n "${NETWORK_NAMESPACE}" patch network "${NETWORK_NAME}" --type merge \
  --patch "{\"spec\":{\"ipFamily\":\"IPv6\",\"operator\":{\"image\":\"${OPERATOR_IMAGE}\"}}}"

kubectl -n "${NETWORK_NAMESPACE}" wait --for=condition=Ready "certificate/self-signed" --timeout=5m
kubectl -n "${NETWORK_NAMESPACE}" wait --for=condition=Ready "network/${NETWORK_NAME}" --timeout=5m
kubectl -n "${NETWORK_NAMESPACE}" patch daemonset "${NETWORK_NAME}" --type merge \
  --patch "${ARM_TEMPLATE_TOLERATION_PATCH}"
kubectl -n "${NETWORK_NAMESPACE}" rollout status "daemonset/${NETWORK_NAME}" --timeout=8m

echo "Waiting for routers to publish IPv6 inner faces"
timeout 5m bash -c '
  set -euo pipefail
  until kubectl -n "$0" get routers -l "network.named-data.net/name=$1" \
      -o jsonpath="{range .items[*]}{.metadata.name}{\" \"}{.status.innerFace}{\"\\n\"}{end}" \
      | tee /tmp/ndn-router-faces.txt \
      | grep -Eq "udp://\\[[0-9a-fA-F:]+"; do
    sleep 5
  done
' "${NETWORK_NAMESPACE}" "${NETWORK_NAME}"

cat /tmp/ndn-router-faces.txt

echo "Applying injected workload examples"
kubectl -n "${WORKLOAD_NAMESPACE}" apply -f "${ROOT_DIR}/examples/workloads/producer-pod.yaml"
kubectl -n "${WORKLOAD_NAMESPACE}" patch pod/test-pingserver --type merge \
  --patch "${ARM_POD_TOLERATION_PATCH}"
kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Ready pod/test-pingserver --timeout=5m

kubectl -n "${WORKLOAD_NAMESPACE}" apply -f "${ROOT_DIR}/examples/workloads/consumer-job.yaml"
timeout 2m bash -c '
  set -euo pipefail
  until kubectl -n "$0" get pods -l job-name=test-ping -o name | grep -q .; do
    sleep 1
  done
' "${WORKLOAD_NAMESPACE}"
kubectl -n "${WORKLOAD_NAMESPACE}" patch pods -l job-name=test-ping --type merge \
  --patch "${ARM_POD_TOLERATION_PATCH}"
kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Complete job/test-ping --timeout=6m

kubectl -n "${WORKLOAD_NAMESPACE}" logs job/test-ping
