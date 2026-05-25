output "cluster_name" {
  description = "Name of the ephemeral GKE cluster."
  value       = google_container_cluster.cluster.name
}

output "cluster_location" {
  description = "Location of the ephemeral GKE cluster."
  value       = google_container_cluster.cluster.location
}

output "network_name" {
  description = "Name of the ephemeral VPC network."
  value       = google_compute_network.vpc.name
}

output "peer_cluster_name" {
  description = "Name of the isolated peer GKE cluster."
  value       = google_container_cluster.peer.name
}

output "peer_cluster_location" {
  description = "Location of the isolated peer GKE cluster."
  value       = google_container_cluster.peer.location
}

output "peer_network_name" {
  description = "Name of the isolated peer VPC network."
  value       = google_compute_network.peer_vpc.name
}
