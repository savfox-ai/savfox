---
name: terraform
description: Manage infrastructure as code with Terraform.
version: "1.0.0"
metadata:
  savfox:
    emoji: "🏗️"
    requires:
      bins:
        - terraform
      env: []
    install:
      - id: brew
        kind: brew
        formula: terraform
        bins: [terraform]
        label: Homebrew
      - id: choco
        kind: choco
        package: terraform
        bins: [terraform]
        label: Chocolatey
---

# Terraform Skill

Manage infrastructure as code.

## Initialize

```bash
terraform init
```

## Plan

Preview changes:
```bash
terraform plan
```

Save plan:
```bash
terraform plan -out=plan.tfplan
```

## Apply

```bash
terraform apply
terraform apply plan.tfplan
```

Auto-approve (CI/CD):
```bash
terraform apply -auto-approve
```

## State

List resources:
```bash
terraform state list
```

Show resource details:
```bash
terraform state show <resource>
```

## Destroy

```bash
terraform destroy
```

Target specific resource:
```bash
terraform destroy -target=aws_instance.example
```

## Output

```bash
terraform output
terraform output -json
```

## Format and Validate

```bash
terraform fmt
terraform validate
```

## Guidelines

- Always run `terraform plan` before `apply`
- Use remote state (S3, GCS) for team workflows
- Never commit `.tfstate` files to version control
- Use variables and modules for reusability
- Use `terraform workspace` for environment separation
