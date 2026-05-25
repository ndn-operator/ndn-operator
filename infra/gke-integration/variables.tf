variable "project_id" {
  description = "Google Cloud project that hosts the ephemeral integration-test cluster."
  type        = string
}

variable "region" {
  description = "Google Cloud region for regional resources such as VPC subnet and Cloud NAT."
  type        = string
  default     = "us-west1"
}

variable "location" {
  description = "GKE cluster location. Use a zone for a cheaper single-zone test cluster."
  type        = string
  default     = "us-west1-a"
}

variable "peer_region" {
  description = "Google Cloud region for the isolated peer cluster networking resources."
  type        = string
  default     = "us-south1"
}

variable "peer_location" {
  description = "GKE location for the isolated peer cluster."
  type        = string
  default     = "us-south1-a"
}

variable "name" {
  description = "Short, unique name prefix for all integration-test resources."
  type        = string

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{0,38}[a-z0-9]$", var.name))
    error_message = "name must be a lowercase RFC1035-ish value between 2 and 40 characters."
  }
}

variable "authorized_cidr" {
  description = "CIDR allowed to reach the public GKE control plane endpoint, normally the GitHub runner public IP as /32."
  type        = string
}

variable "subnet_cidr" {
  description = "Primary IPv4 CIDR for GKE nodes."
  type        = string
  default     = "10.42.0.0/20"
}

variable "pods_cidr" {
  description = "Secondary IPv4 CIDR for GKE pods."
  type        = string
  default     = "10.44.0.0/14"
}

variable "services_cidr" {
  description = "Secondary IPv4 CIDR for Kubernetes services."
  type        = string
  default     = "10.48.0.0/20"
}

variable "master_ipv4_cidr_block" {
  description = "Private IPv4 /28 used by the GKE control plane."
  type        = string
  default     = "172.16.0.0/28"
}

variable "peer_subnet_cidr" {
  description = "Primary IPv4 CIDR for the isolated peer GKE nodes."
  type        = string
  default     = "10.52.0.0/20"
}

variable "peer_pods_cidr" {
  description = "Secondary IPv4 CIDR for isolated peer GKE pods."
  type        = string
  default     = "10.56.0.0/14"
}

variable "peer_services_cidr" {
  description = "Secondary IPv4 CIDR for isolated peer Kubernetes services."
  type        = string
  default     = "10.60.0.0/20"
}

variable "peer_master_ipv4_cidr_block" {
  description = "Private IPv4 /28 used by the isolated peer GKE control plane."
  type        = string
  default     = "172.16.0.16/28"
}

variable "ipv6_access_type" {
  description = "IPv6 access type for the dual-stack subnet."
  type        = string
  default     = "EXTERNAL"

  validation {
    condition     = contains(["EXTERNAL", "INTERNAL"], var.ipv6_access_type)
    error_message = "ipv6_access_type must be EXTERNAL or INTERNAL."
  }
}

variable "machine_type" {
  description = "Machine type for the GKE node pool."
  type        = string
  default     = "n4a-standard-1"
}

variable "peer_machine_type" {
  description = "Machine type for the isolated peer GKE node pool."
  type        = string
  default     = "e2-standard-2"
}

variable "node_count" {
  description = "Number of nodes in the test node pool."
  type        = number
  default     = 3
}

variable "peer_node_count" {
  description = "Number of nodes in the isolated peer test node pool."
  type        = number
  default     = 1
}

variable "disk_size_gb" {
  description = "Boot disk size for GKE nodes."
  type        = number
  default     = 50
}

variable "release_channel" {
  description = "GKE release channel."
  type        = string
  default     = "REGULAR"
}

variable "pr_number" {
  description = "Pull request number that requested the integration run."
  type        = string
}

variable "run_id" {
  description = "GitHub Actions run id."
  type        = string
}

variable "commit_sha" {
  description = "Pull request head commit SHA under test."
  type        = string
}
