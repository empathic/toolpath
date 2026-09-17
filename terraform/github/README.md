# GitHub repository settings

This directory manages settings on `empathic/toolpath` with the `integrations/github` Terraform provider.

Managed settings:

- `delete_branch_on_merge`: GitHub deletes the head branch after a pull request merges.

The `import` block adopts the existing repository into state. Terraform does not create or delete the repository. `prevent_destroy` blocks a destroy, `archive_on_destroy` archives instead of deleting if the resource is removed from the config, and `ignore_changes` lists the attributes the GitHub UI still owns. An attribute leaves `ignore_changes` when it moves under management.

## Files

- `versions.tf`: Terraform and provider version constraints.
- `providers.tf`: provider configuration.
- `variables.tf`: owner and repository name.
- `repository.tf`: the repository resource and its import.

## Run

The provider authenticates with `GITHUB_TOKEN`. The token needs `administration:write` on the repository.

```bash
export GITHUB_TOKEN="$(gh auth token)"
```

```bash
cd terraform/github && terraform init
```

```bash
cd terraform/github && terraform plan
```

The first plan shows one import and one in-place update. Apply only when the plan shows nothing else.

```bash
cd terraform/github && terraform apply
```

State is local (`terraform.tfstate`) and gitignored.
