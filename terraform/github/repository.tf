import {
  to = github_repository.this
  id = var.repository
}

resource "github_repository" "this" {
  name = var.repository

  delete_branch_on_merge = true

  archive_on_destroy = true

  # Attributes the GitHub UI still owns. A plan must not propose changes
  # to them.
  lifecycle {
    prevent_destroy = true
    ignore_changes = [
      description,
      homepage_url,
      visibility,
      topics,
      has_issues,
      has_discussions,
      has_projects,
      has_wiki,
      has_downloads,
      is_template,
      allow_merge_commit,
      allow_squash_merge,
      allow_rebase_merge,
      allow_auto_merge,
      allow_update_branch,
      squash_merge_commit_title,
      squash_merge_commit_message,
      merge_commit_title,
      merge_commit_message,
      web_commit_signoff_required,
      vulnerability_alerts,
      security_and_analysis,
      pages,
      template,
      archived,
      auto_init,
      gitignore_template,
      license_template,
    ]
  }
}
