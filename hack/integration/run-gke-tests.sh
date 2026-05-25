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
PEER_NETWORK_NAME="${PEER_NETWORK_NAME:-test}"
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
  # Generated injector TLS is rotated by Helm upgrades; restart to load the new serving certificate.
  kubectl --kubeconfig "${kubeconfig}" -n "${OPERATOR_NAMESPACE}" \
    rollout restart deployment/ndn-injector
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

import_certificate_material() {
  local source_kubeconfig="$1"
  local source_namespace="$2"
  local source_certificate="$3"
  local destination_kubeconfig="$4"
  local destination_namespace="$5"
  local destination_secret="$6"
  local include_key="${7:-false}"
  local source_secret
  local encoded_certificate
  local certificate_text
  local key_secret
  local encoded_key
  local key_text
  local -a secret_args

  source_secret="$(kubectl --kubeconfig "${source_kubeconfig}" -n "${source_namespace}" \
    get certificate "${source_certificate}" -o jsonpath='{.status.cert.secret}')"
  encoded_certificate="$(kubectl --kubeconfig "${source_kubeconfig}" -n "${source_namespace}" \
    get secret "${source_secret}" -o jsonpath='{.data.ndn\.cert}')"
  certificate_text="$(printf '%s' "${encoded_certificate}" | base64 --decode)"

  secret_args=(
    kubectl --kubeconfig "${destination_kubeconfig}" -n "${destination_namespace}"
    create secret generic "${destination_secret}"
    --from-literal="ndn.cert=${certificate_text}"
  )
  if [[ "${include_key}" == "true" ]]; then
    key_secret="$(kubectl --kubeconfig "${source_kubeconfig}" -n "${source_namespace}" \
      get certificate "${source_certificate}" -o jsonpath='{.status.key.secret}')"
    encoded_key="$(kubectl --kubeconfig "${source_kubeconfig}" -n "${source_namespace}" \
      get secret "${key_secret}" -o jsonpath='{.data.ndn\.key}')"
    key_text="$(printf '%s' "${encoded_key}" | base64 --decode)"
    secret_args+=(--from-literal="ndn.key=${key_text}")
  fi

  "${secret_args[@]}" --dry-run=client -o yaml \
    | kubectl --kubeconfig "${destination_kubeconfig}" apply -f -
}

assert_verified_data() {
  local cluster_name="$1"
  local job_logs="$2"
  echo "${cluster_name} consumer output:"
  printf '%s\n' "${job_logs}"
  if [[ "${job_logs}" != *"VERIFIED data /root-network/subnetwork1/helloworld/valid:"* ]]; then
    echo "${cluster_name} consumer did not validate signed Data" >&2
    return 1
  fi
}

assert_rejected_data() {
  local cluster_name="$1"
  local job_logs="$2"
  echo "${cluster_name} rejection output:"
  printf '%s\n' "${job_logs}"
  if [[ "${job_logs}" != *"REJECTED data /root-network/subnetwork1/helloworld/forged:"* ]]; then
    echo "${cluster_name} consumer did not reject sibling-prefix impersonation" >&2
    return 1
  fi
}

wait_for_network_rollout() {
  local kubeconfig="$1"
  local network_name="$2"
  local deadline=$((SECONDS + 300))
  local image

  while true; do
    image="$(kubectl --kubeconfig "${kubeconfig}" -n "${NETWORK_NAMESPACE}" \
      get daemonset "${network_name}" \
      -o jsonpath='{.spec.template.spec.initContainers[0].image}' 2>/dev/null || true)"
    if [[ "${image}" == "${OPERATOR_IMAGE}" ]]; then
      break
    fi
    if (( SECONDS >= deadline )); then
      echo "Timed out waiting for daemonset/${network_name} to use ${OPERATOR_IMAGE}; found ${image:-<none>}" >&2
      return 1
    fi
    sleep 5
  done
  kubectl --kubeconfig "${kubeconfig}" -n "${NETWORK_NAMESPACE}" \
    rollout status "daemonset/${network_name}" --timeout=8m
}

wait_for_ipv6_inner_faces() {
  local deadline=$((SECONDS + 300))
  local faces

  while true; do
    faces="$(primary_kubectl -n "${NETWORK_NAMESPACE}" get routers \
      -l "network.named-data.net/name=${NETWORK_NAME}" \
      -o jsonpath='{range .items[*]}{.metadata.name}{" "}{.status.innerFace}{"\n"}{end}' 2>/dev/null || true)"
    if printf '%s\n' "${faces}" | grep -Eq 'udp://\[[0-9a-fA-F:]+'; then
      printf '%s\n' "${faces}"
      return 0
    fi
    if (( SECONDS >= deadline )); then
      echo "Timed out waiting for primary routers to publish IPv6 inner faces" >&2
      printf '%s\n' "${faces}" >&2
      return 1
    fi
    sleep 5
  done
}

wait_for_tcp_address() {
  local deadline=$((SECONDS + 600))
  local address

  while true; do
    address="$(primary_kubectl -n "${NETWORK_NAMESPACE}" get service "${NETWORK_NAME}-tcp" \
      -o jsonpath='{.status.loadBalancer.ingress[0].ip}' 2>/dev/null || true)"
    if [[ -z "${address}" ]]; then
      address="$(primary_kubectl -n "${NETWORK_NAMESPACE}" get service "${NETWORK_NAME}-tcp" \
        -o jsonpath='{.status.loadBalancer.ingress[0].hostname}' 2>/dev/null || true)"
    fi
    if [[ -n "${address}" ]]; then
      printf '%s' "${address}"
      return 0
    fi
    if (( SECONDS >= deadline )); then
      echo "Timed out waiting for service/${NETWORK_NAME}-tcp to receive an external address" >&2
      return 1
    fi
    sleep 5
  done
}

wait_for_peer_outer_neighbor() {
  local uri="$1"
  local deadline=$((SECONDS + 300))
  local neighbors

  while true; do
    neighbors="$(peer_kubectl -n "${NETWORK_NAMESPACE}" get routers \
      -l "network.named-data.net/name=${PEER_NETWORK_NAME}" \
      -o jsonpath='{range .items[*]}{.status.outerNeighbors.to-test}{"\n"}{end}' 2>/dev/null || true)"
    if printf '%s\n' "${neighbors}" | grep -Fqx -- "${uri}"; then
      return 0
    fi
    if (( SECONDS >= deadline )); then
      echo "Timed out waiting for peer routers to receive outer neighbor ${uri}" >&2
      printf '%s\n' "${neighbors}" >&2
      return 1
    fi
    sleep 5
  done
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
import_certificate_material "${PRIMARY_KUBECONFIG}" "${NETWORK_NAMESPACE}" "${PRIMARY_ROOT_NAME}" \
  "${PEER_KUBECONFIG}" "${NETWORK_NAMESPACE}" "primary-root"
import_certificate_material "${PEER_KUBECONFIG}" "${NETWORK_NAMESPACE}" "${PEER_ROOT_NAME}" \
  "${PRIMARY_KUBECONFIG}" "${NETWORK_NAMESPACE}" "peer-root"
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
  --patch "{\"spec\":{\"ipFamily\":\"IPv6\",\"operator\":{\"image\":\"${OPERATOR_IMAGE}\"},\"trustAnchors\":[{\"name\":\"${PRIMARY_ROOT_NAME}\",\"kind\":\"Certificate\"},{\"name\":\"peer-root\",\"kind\":\"ExternalCertificate\"}],\"faces\":{\"tcp\":{\"port\":6363,\"serviceTemplate\":{\"spec\":{\"type\":\"LoadBalancer\"}}}}}}"
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
wait_for_ipv6_inner_faces

echo "Creating application signing and validation credentials"
primary_kubectl -n "${WORKLOAD_NAMESPACE}" apply \
  -f "${ROOT_DIR}/examples/workloads/application-certificates.yaml"
primary_kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Ready \
  certificate/app-root certificate/app-subnetwork1-ca certificate/app-subnetwork1-helloworld \
  certificate/app-subnetwork2-ca certificate/app-subnetwork2-helloworld --timeout=5m

echo "Exporting the public application chain and one deliberate forging signer to the peer"
import_certificate_material "${PRIMARY_KUBECONFIG}" "${WORKLOAD_NAMESPACE}" "app-root" \
  "${PEER_KUBECONFIG}" "${WORKLOAD_NAMESPACE}" "app-root"
import_certificate_material "${PRIMARY_KUBECONFIG}" "${WORKLOAD_NAMESPACE}" "app-subnetwork1-ca" \
  "${PEER_KUBECONFIG}" "${WORKLOAD_NAMESPACE}" "app-subnetwork1-ca"
import_certificate_material "${PRIMARY_KUBECONFIG}" "${WORKLOAD_NAMESPACE}" "app-subnetwork1-helloworld" \
  "${PEER_KUBECONFIG}" "${WORKLOAD_NAMESPACE}" "app-subnetwork1-helloworld"
import_certificate_material "${PRIMARY_KUBECONFIG}" "${WORKLOAD_NAMESPACE}" "app-subnetwork2-ca" \
  "${PEER_KUBECONFIG}" "${WORKLOAD_NAMESPACE}" "app-subnetwork2-ca"
import_certificate_material "${PRIMARY_KUBECONFIG}" "${WORKLOAD_NAMESPACE}" "app-subnetwork2-helloworld" \
  "${PEER_KUBECONFIG}" "${WORKLOAD_NAMESPACE}" "app-subnetwork2-helloworld" true
peer_kubectl -n "${WORKLOAD_NAMESPACE}" apply \
  -f "${ROOT_DIR}/hack/integration/fixtures/peer-app-certificates.yaml"
peer_kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Ready \
  externalcertificate/app-root externalcertificate/app-subnetwork1-ca \
  externalcertificate/app-subnetwork1-helloworld externalcertificate/app-subnetwork2-ca \
  externalcertificate/app-subnetwork2-helloworld --timeout=5m

echo "Applying signed primary producer and same-cluster validating consumer"
primary_kubectl -n "${WORKLOAD_NAMESPACE}" apply -f "${ROOT_DIR}/examples/workloads/producer-pod.yaml"
primary_kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Ready pod/test-helloworld-producer --timeout=5m
primary_kubectl -n "${WORKLOAD_NAMESPACE}" apply -f "${ROOT_DIR}/examples/workloads/consumer-job.yaml"
primary_kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Complete job/test-helloworld-consumer --timeout=6m
primary_logs="$(primary_kubectl -n "${WORKLOAD_NAMESPACE}" logs job/test-helloworld-consumer)"
assert_verified_data "Primary" "${primary_logs}"

echo "Waiting for the primary public TCP face"
tcp_address="$(wait_for_tcp_address)"
tcp_host="${tcp_address}"
if [[ "${tcp_host}" == *:* && "${tcp_host}" != \[*\] ]]; then
  tcp_host="[${tcp_host}]"
fi
tcp_uri="tcp://${tcp_host}:6363"
echo "Connecting peer network to ${tcp_uri}"
peer_kubectl -n "${NETWORK_NAMESPACE}" apply -f - <<EOF
apiVersion: named-data.net/v1alpha1
kind: Neighbor
metadata:
  name: to-test
spec:
  network: ${PEER_NETWORK_NAME}
  uri: "${tcp_uri}"
EOF

echo "Waiting for the peer router to receive the external neighbor"
wait_for_peer_outer_neighbor "${tcp_uri}"

echo "Applying cross-cluster validating consumer"
peer_kubectl -n "${WORKLOAD_NAMESPACE}" apply \
  -f "${ROOT_DIR}/hack/integration/fixtures/peer-consumer-job.yaml"
peer_kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Complete job/peer-helloworld-consumer --timeout=6m
peer_logs="$(peer_kubectl -n "${WORKLOAD_NAMESPACE}" logs job/peer-helloworld-consumer)"
assert_verified_data "Peer" "${peer_logs}"

echo "Applying peer sibling-prefix impersonation attempt"
peer_kubectl -n "${WORKLOAD_NAMESPACE}" apply \
  -f "${ROOT_DIR}/hack/integration/fixtures/peer-forger-pod.yaml"
peer_kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Ready pod/peer-helloworld-forger --timeout=5m
peer_kubectl -n "${WORKLOAD_NAMESPACE}" apply \
  -f "${ROOT_DIR}/hack/integration/fixtures/peer-rejection-job.yaml"
peer_kubectl -n "${WORKLOAD_NAMESPACE}" wait --for=condition=Complete job/peer-rejects-sibling-forgery --timeout=6m
rejection_logs="$(peer_kubectl -n "${WORKLOAD_NAMESPACE}" logs job/peer-rejects-sibling-forgery)"
assert_rejected_data "Peer" "${rejection_logs}"
