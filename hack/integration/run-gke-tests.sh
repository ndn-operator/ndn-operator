#!/usr/bin/env bash
set -euo pipefail

DEFAULT_ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ROOT_DIR="${REPO_UNDER_TEST_DIR:-${DEFAULT_ROOT_DIR}}"

: "${OPERATOR_IMAGE:?OPERATOR_IMAGE must be set, for example ghcr.io/owner/repo:pr-123}"
: "${PRIMARY_KUBECONFIG:?PRIMARY_KUBECONFIG must point to the primary cluster kubeconfig}"
: "${PEER_KUBECONFIG:?PEER_KUBECONFIG must point to the peer cluster kubeconfig}"

OPERATOR_NAMESPACE="${OPERATOR_NAMESPACE:-ndn-operator}"
NETWORK_NAMESPACE="${NETWORK_NAMESPACE:-mynetwork}"
WORKLOAD_NAMESPACE="${WORKLOAD_NAMESPACE:-ndn-workloads}"
NETWORK_NAME="${NETWORK_NAME:-test}"
PEER_NETWORK_NAME="${PEER_NETWORK_NAME:-peer}"
PRIMARY_ROOT_NAME="${PRIMARY_ROOT_NAME:-self-signed}"
PEER_ROOT_NAME="${PEER_ROOT_NAME:-peer-root}"
ARM_TOLERATION='[{"key":"kubernetes.io/arch","operator":"Equal","value":"arm64","effect":"NoSchedule"}]'

if [[ "${OPERATOR_IMAGE}" != *:* ]]; then
  echo "OPERATOR_IMAGE must include a tag: ${OPERATOR_IMAGE}" >&2
  exit 1
fi

IMAGE_REPOSITORY="${OPERATOR_IMAGE%:*}"
IMAGE_TAG="${OPERATOR_IMAGE##*:}"
LOW_CPU_REQUEST=25m

primary_kubectl() {
  kubectl --kubeconfig "${PRIMARY_KUBECONFIG}" "$@"
}

peer_kubectl() {
  kubectl --kubeconfig "${PEER_KUBECONFIG}" "$@"
}

install_operator() {
  local cluster_name="$1"
  local kubeconfig="$2"

  echo "Installing ndn-operator from ${OPERATOR_IMAGE} in ${cluster_name}"
  helm --kubeconfig "${kubeconfig}" upgrade --install ndn-operator "${ROOT_DIR}/charts/ndn-operator" \
    --namespace "${OPERATOR_NAMESPACE}" \
    --create-namespace \
    --set "image.repository=${IMAGE_REPOSITORY}" \
    --set "image.tag=${IMAGE_TAG}" \
    --set "image.pullPolicy=Always" \
    --set "controllers.network.resources.requests.cpu=${LOW_CPU_REQUEST}" \
    --set "controllers.router.resources.requests.cpu=${LOW_CPU_REQUEST}" \
    --set "controllers.neighbor.resources.requests.cpu=${LOW_CPU_REQUEST}" \
    --set "controllers.pod.resources.requests.cpu=${LOW_CPU_REQUEST}" \
    --set "controllers.certificate.resources.requests.cpu=${LOW_CPU_REQUEST}" \
    --set "controllers.externalCertificate.resources.requests.cpu=${LOW_CPU_REQUEST}" \
    --set "injector.resources.requests.cpu=${LOW_CPU_REQUEST}" \
    --set-json "tolerations=${ARM_TOLERATION}" \
    --wait \
    --timeout 5m

  kubectl --kubeconfig "${kubeconfig}" -n "${OPERATOR_NAMESPACE}" \
    rollout status deployment/ndn-controller --timeout=5m
  kubectl --kubeconfig "${kubeconfig}" -n "${OPERATOR_NAMESPACE}" \
    rollout status deployment/ndn-injector --timeout=5m
}

create_namespaces() {
  local kubeconfig="$1"
  kubectl --kubeconfig "${kubeconfig}" create namespace "${NETWORK_NAMESPACE}" \
    --dry-run=client -o yaml | kubectl --kubeconfig "${kubeconfig}" apply -f -
  kubectl --kubeconfig "${kubeconfig}" create namespace "${WORKLOAD_NAMESPACE}" \
    --dry-run=client -o yaml | kubectl --kubeconfig "${kubeconfig}" apply -f -
}

import_public_root() {
  local source_kubeconfig="$1"
  local source_certificate="$2"
  local destination_kubeconfig="$3"
  local destination_secret="$4"
  local source_secret
  local encoded_certificate
  local certificate_text

  source_secret="$(kubectl --kubeconfig "${source_kubeconfig}" -n "${NETWORK_NAMESPACE}" \
    get certificate "${source_certificate}" -o jsonpath='{.status.cert.secret}')"
  encoded_certificate="$(kubectl --kubeconfig "${source_kubeconfig}" -n "${NETWORK_NAMESPACE}" \
    get secret "${source_secret}" -o jsonpath='{.data.ndn\.cert}')"
  certificate_text="$(printf '%s' "${encoded_certificate}" | base64 --decode)"

  kubectl --kubeconfig "${destination_kubeconfig}" -n "${NETWORK_NAMESPACE}" \
    create secret generic "${destination_secret}" \
    --from-literal="ndn.cert=${certificate_text}" --dry-run=client -o yaml \
    | kubectl --kubeconfig "${destination_kubeconfig}" apply -f -
}

assert_ping_received_data() {
  local cluster_name="$1"
  local job_logs="$2"
  echo "${cluster_name} consumer output:"
  printf '%s\n' "${job_logs}"
  if [[ "${job_logs}" != *"content from /test/pingserver:"* ]]; then
    echo "${cluster_name} consumer received no Data packets" >&2
    return 1
  fi
}

wait_for_network_rollout() {
  local kubeconfig="$1"
  local network_name="$2"

  timeout 5m bash -c '
    set -euo pipefail
    until [[ "$(kubectl --kubeconfig "$0" -n "$1" get daemonset "$2" \
        -o jsonpath="{.spec.template.spec.initContainers[0].image}" 2>/dev/null)" == "$3" ]]; do
      sleep 5
    done
  ' "${kubeconfig}" "${NETWORK_NAMESPACE}" "${network_name}" "${OPERATOR_IMAGE}"
  kubectl --kubeconfig "${kubeconfig}" -n "${NETWORK_NAMESPACE}" \
    rollout status "daemonset/${network_name}" --timeout=8m
}

install_operator "primary cluster" "${PRIMARY_KUBECONFIG}"
install_operator "peer cluster" "${PEER_KUBECONFIG}"
create_namespaces "${PRIMARY_KUBECONFIG}"
create_namespaces "${PEER_KUBECONFIG}"

echo "Creating local root certificates in both clusters"
primary_kubectl -n "${NETWORK_NAMESPACE}" apply -f "${ROOT_DIR}/examples/secure/self-signed-cert.yaml"
peer_kubectl -n "${NETWORK_NAMESPACE}" apply \
  -f "${ROOT_DIR}/hack/integration/fixtures/peer-self-signed-cert.yaml"
primary_kubectl -n "${NETWORK_NAMESPACE}" wait --for=condition=Ready \
  "certificate/${PRIMARY_ROOT_NAME}" --timeout=5m
peer_kubectl -n "${NETWORK_NAMESPACE}" wait --for=condition=Ready \
  "certificate/${PEER_ROOT_NAME}" --timeout=5m

echo "Exchanging public trust anchors between clusters"
import_public_root "${PRIMARY_KUBECONFIG}" "${PRIMARY_ROOT_NAME}" "${PEER_KUBECONFIG}" "primary-root"
import_public_root "${PEER_KUBECONFIG}" "${PEER_ROOT_NAME}" "${PRIMARY_KUBECONFIG}" "peer-root"
primary_kubectl -n "${NETWORK_NAMESPACE}" apply \
  -f "${ROOT_DIR}/hack/integration/fixtures/primary-peer-root.yaml"
peer_kubectl -n "${NETWORK_NAMESPACE}" apply \
  -f "${ROOT_DIR}/hack/integration/fixtures/peer-primary-root.yaml"
primary_kubectl -n "${NETWORK_NAMESPACE}" wait --for=condition=Ready \
  externalcertificate/peer-root --timeout=5m
peer_kubectl -n "${NETWORK_NAMESPACE}" wait --for=condition=Ready \
  externalcertificate/primary-root --timeout=5m

echo "Applying secured primary and peer NDN networks"
primary_kubectl -n "${NETWORK_NAMESPACE}" apply -f "${ROOT_DIR}/examples/secure/network.yaml"
primary_kubectl -n "${NETWORK_NAMESPACE}" patch network "${NETWORK_NAME}" --type merge \
  --patch "{\"spec\":{\"ipFamily\":\"IPv6\",\"operator\":{\"image\":\"${OPERATOR_IMAGE}\"},\"trustAnchors\":[{\"name\":\"${PRIMARY_ROOT_NAME}\",\"kind\":\"Certificate\"},{\"name\":\"peer-root\",\"kind\":\"ExternalCertificate\"}],\"faces\":{\"websocket\":{\"port\":9696,\"serviceTemplate\":{\"spec\":{\"type\":\"LoadBalancer\"}}}}}}"
peer_kubectl -n "${NETWORK_NAMESPACE}" apply \
  -f "${ROOT_DIR}/hack/integration/fixtures/peer-network.yaml"
peer_kubectl -n "${NETWORK_NAMESPACE}" patch network "${PEER_NETWORK_NAME}" --type merge \
  --patch "{\"spec\":{\"operator\":{\"image\":\"${OPERATOR_IMAGE}\"}}}"

primary_kubectl -n "${NETWORK_NAMESPACE}" wait --for=condition=Ready \
  "network/${NETWORK_NAME}" --timeout=5m
peer_kubectl -n "${NETWORK_NAMESPACE}" wait --for=condition=Ready \
  "network/${PEER_NETWORK_NAME}" --timeout=5m
wait_for_network_rollout "${PRIMARY_KUBECONFIG}" "${NETWORK_NAME}"
wait_for_network_rollout "${PEER_KUBECONFIG}" "${PEER_NETWORK_NAME}"

echo "Waiting for routers to publish IPv6 inner faces"
timeout 5m bash -c '
  set -euo pipefail
  until kubectl --kubeconfig "$0" -n "$1" get routers -l "network.named-data.net/name=$2" \
      -o jsonpath="{range .items[*]}{.metadata.name}{\" \"}{.status.innerFace}{\"\\n\"}{end}" \
      | tee /tmp/ndn-primary-router-faces.txt \
      | grep -Eq "udp://\\[[0-9a-fA-F:]+"; do
    sleep 5
  done
' "${PRIMARY_KUBECONFIG}" "${NETWORK_NAMESPACE}" "${NETWORK_NAME}"

cat /tmp/ndn-primary-router-faces.txt

echo "Applying primary producer and same-cluster consumer"
primary_kubectl -n "${WORKLOAD_NAMESPACE}" apply -f "${ROOT_DIR}/examples/workloads/producer-pod.yaml"
primary_kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Ready pod/test-pingserver --timeout=5m
primary_kubectl -n "${WORKLOAD_NAMESPACE}" apply -f "${ROOT_DIR}/examples/workloads/consumer-job.yaml"
primary_kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Complete job/test-ping --timeout=6m
primary_logs="$(primary_kubectl -n "${WORKLOAD_NAMESPACE}" logs job/test-ping)"
assert_ping_received_data "Primary" "${primary_logs}"

echo "Waiting for the primary public WebSocket face"
ws_address="$(
  timeout 10m bash -c '
    set -euo pipefail
    until address="$(kubectl --kubeconfig "$0" -n "$1" get service "$2" \
        -o jsonpath="{.status.loadBalancer.ingress[0].ip}" 2>/dev/null)" \
        && [[ -n "${address}" ]]; do
      address="$(kubectl --kubeconfig "$0" -n "$1" get service "$2" \
        -o jsonpath="{.status.loadBalancer.ingress[0].hostname}" 2>/dev/null || true)"
      if [[ -n "${address}" ]]; then
        break
      fi
      sleep 5
    done
    printf "%s" "${address}"
  ' "${PRIMARY_KUBECONFIG}" "${NETWORK_NAMESPACE}" "${NETWORK_NAME}-ws"
)"
ws_host="${ws_address}"
if [[ "${ws_host}" == *:* && "${ws_host}" != \[*\] ]]; then
  ws_host="[${ws_host}]"
fi
ws_uri="ws://${ws_host}:9696"
echo "Connecting peer network to ${ws_uri}"
peer_kubectl -n "${NETWORK_NAMESPACE}" apply -f - <<EOF
apiVersion: named-data.net/v1alpha1
kind: Neighbor
metadata:
  name: to-test
spec:
  network: ${PEER_NETWORK_NAME}
  uri: "${ws_uri}"
EOF

echo "Waiting for the peer router to receive the external neighbor"
timeout 5m bash -c '
  set -euo pipefail
  until kubectl --kubeconfig "$0" -n "$1" get routers -l "network.named-data.net/name=$2" \
      -o jsonpath="{range .items[*]}{range .status.outerNeighbors[*]}{.}{\"\\n\"}{end}{end}" \
      | grep -Fx -- "$3"; do
    sleep 5
  done
' "${PEER_KUBECONFIG}" "${NETWORK_NAMESPACE}" "${PEER_NETWORK_NAME}" "${ws_uri}"

echo "Applying cross-cluster consumer"
peer_kubectl -n "${WORKLOAD_NAMESPACE}" apply \
  -f "${ROOT_DIR}/hack/integration/fixtures/peer-consumer-job.yaml"
peer_kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Complete job/peer-test-ping --timeout=6m
peer_logs="$(peer_kubectl -n "${WORKLOAD_NAMESPACE}" logs job/peer-test-ping)"
assert_ping_received_data "Peer" "${peer_logs}"
