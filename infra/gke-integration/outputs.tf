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
