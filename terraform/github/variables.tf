variable "owner" {
  description = "GitHub organization that owns the repository."
  type        = string
  default     = "empathic"
}

variable "repository" {
  description = "Repository name, without the owner."
  type        = string
  default     = "toolpath"
}
