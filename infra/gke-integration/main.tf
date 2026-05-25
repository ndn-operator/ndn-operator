locals {
  node_tag      = "${var.name}-node"
  peer_name     = "${substr(var.name, 0, 35)}-peer"
  peer_node_tag = "${substr(var.name, 0, 35)}-peer-node"

  labels = {
    app     = "ndn-operator"
    purpose = "gke-integration"
    pr      = substr(var.pr_number, 0, 63)
    run     = substr(var.run_id, 0, 63)
    commit  = substr(lower(var.commit_sha), 0, 40)
  }
}

resource "google_compute_network" "vpc" {
  name                    = "${var.name}-vpc"
  auto_create_subnetworks = false
}

resource "google_compute_subnetwork" "gke" {
  provider = google-beta

  name                     = "${var.name}-subnet"
  region                   = var.region
  network                  = google_compute_network.vpc.id
  ip_cidr_range            = var.subnet_cidr
  private_ip_google_access = true
  stack_type               = "IPV4_IPV6"
  ipv6_access_type         = var.ipv6_access_type

  secondary_ip_range {
    range_name    = "pods"
    ip_cidr_range = var.pods_cidr
  }

  secondary_ip_range {
    range_name    = "services"
    ip_cidr_range = var.services_cidr
  }
}

resource "google_compute_router" "router" {
  name    = "${var.name}-router"
  region  = var.region
  network = google_compute_network.vpc.id
}

resource "google_compute_router_nat" "nat" {
  name                               = "${var.name}-nat"
  router                             = google_compute_router.router.name
  region                             = var.region
  nat_ip_allocate_option             = "AUTO_ONLY"
  source_subnetwork_ip_ranges_to_nat = "LIST_OF_SUBNETWORKS"

  subnetwork {
    name                    = google_compute_subnetwork.gke.id
    source_ip_ranges_to_nat = ["ALL_IP_RANGES"]
  }

  log_config {
    enable = true
    filter = "ERRORS_ONLY"
  }
}

resource "google_container_cluster" "cluster" {
  provider = google-beta

  name     = var.name
  location = var.location

  network           = google_compute_network.vpc.id
  subnetwork        = google_compute_subnetwork.gke.id
  networking_mode   = "VPC_NATIVE"
  datapath_provider = "ADVANCED_DATAPATH"

  remove_default_node_pool = true
  initial_node_count       = 1
  deletion_protection      = false
  enable_shielded_nodes    = true
  resource_labels          = local.labels

  ip_allocation_policy {
    cluster_secondary_range_name  = "pods"
    services_secondary_range_name = "services"
    stack_type                    = "IPV4_IPV6"
  }

  private_cluster_config {
    enable_private_nodes    = true
    enable_private_endpoint = false
    master_ipv4_cidr_block  = var.master_ipv4_cidr_block
  }

  master_authorized_networks_config {
    cidr_blocks {
      cidr_block   = var.authorized_cidr
      display_name = "github-runner"
    }
  }

  release_channel {
    channel = var.release_channel
  }

  depends_on = [
    google_compute_router_nat.nat,
  ]
}

resource "google_container_node_pool" "primary" {
  provider = google-beta

  name     = "primary"
  cluster  = google_container_cluster.cluster.name
  location = google_container_cluster.cluster.location

  node_count = var.node_count

  management {
    auto_repair  = true
    auto_upgrade = true
  }

  node_config {
    preemptible  = true
    machine_type = var.machine_type
    disk_size_gb = var.disk_size_gb
    image_type   = "COS_CONTAINERD"
    tags         = [local.node_tag]
    labels       = local.labels

    metadata = {
      disable-legacy-endpoints = "true"
    }

    oauth_scopes = [
      "https://www.googleapis.com/auth/cloud-platform",
    ]

    shielded_instance_config {
      enable_secure_boot          = true
      enable_integrity_monitoring = true
    }
  }
}

resource "google_compute_network" "peer_vpc" {
  name                    = "${local.peer_name}-vpc"
  auto_create_subnetworks = false
}

resource "google_compute_subnetwork" "peer_gke" {
  provider = google-beta

  name                     = "${local.peer_name}-subnet"
  region                   = var.peer_region
  network                  = google_compute_network.peer_vpc.id
  ip_cidr_range            = var.peer_subnet_cidr
  private_ip_google_access = true

  secondary_ip_range {
    range_name    = "pods"
    ip_cidr_range = var.peer_pods_cidr
  }

  secondary_ip_range {
    range_name    = "services"
    ip_cidr_range = var.peer_services_cidr
  }
}

resource "google_compute_router" "peer_router" {
  name    = "${local.peer_name}-router"
  region  = var.peer_region
  network = google_compute_network.peer_vpc.id
}

resource "google_compute_router_nat" "peer_nat" {
  name                               = "${local.peer_name}-nat"
  router                             = google_compute_router.peer_router.name
  region                             = var.peer_region
  nat_ip_allocate_option             = "AUTO_ONLY"
  source_subnetwork_ip_ranges_to_nat = "LIST_OF_SUBNETWORKS"

  subnetwork {
    name                    = google_compute_subnetwork.peer_gke.id
    source_ip_ranges_to_nat = ["ALL_IP_RANGES"]
  }

  log_config {
    enable = true
    filter = "ERRORS_ONLY"
  }
}

resource "google_container_cluster" "peer" {
  provider = google-beta

  name     = local.peer_name
  location = var.peer_location

  network           = google_compute_network.peer_vpc.id
  subnetwork        = google_compute_subnetwork.peer_gke.id
  networking_mode   = "VPC_NATIVE"
  datapath_provider = "ADVANCED_DATAPATH"

  remove_default_node_pool = true
  initial_node_count       = 1
  deletion_protection      = false
  enable_shielded_nodes    = true
  resource_labels          = local.labels

  ip_allocation_policy {
    cluster_secondary_range_name  = "pods"
    services_secondary_range_name = "services"
    stack_type                    = "IPV4"
  }

  private_cluster_config {
    enable_private_nodes    = true
    enable_private_endpoint = false
    master_ipv4_cidr_block  = var.peer_master_ipv4_cidr_block
  }

  master_authorized_networks_config {
    cidr_blocks {
      cidr_block   = var.authorized_cidr
      display_name = "github-runner"
    }
  }

  release_channel {
    channel = var.release_channel
  }

  depends_on = [
    google_compute_router_nat.peer_nat,
  ]
}

resource "google_container_node_pool" "peer" {
  provider = google-beta

  name     = "primary"
  cluster  = google_container_cluster.peer.name
  location = google_container_cluster.peer.location

  node_count = var.peer_node_count

  management {
    auto_repair  = true
    auto_upgrade = true
  }

  node_config {
    preemptible  = true
    machine_type = var.peer_machine_type
    disk_size_gb = var.disk_size_gb
    image_type   = "COS_CONTAINERD"
    tags         = [local.peer_node_tag]
    labels       = local.labels

    metadata = {
      disable-legacy-endpoints = "true"
    }

    oauth_scopes = [
      "https://www.googleapis.com/auth/cloud-platform",
    ]

    shielded_instance_config {
      enable_secure_boot          = true
      enable_integrity_monitoring = true
    }
  }
}
