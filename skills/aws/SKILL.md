---
name: aws
description: Manage AWS resources using the AWS CLI.
version: "1.0.0"
metadata:
  savfox:
    emoji: "☁️"
    requires:
      bins:
        - aws
      env:
        - AWS_ACCESS_KEY_ID
        - AWS_SECRET_ACCESS_KEY
    install:
      - id: brew
        kind: brew
        formula: awscli
        bins: [aws]
        label: Homebrew
      - id: choco
        kind: choco
        package: awscli
        bins: [aws]
        label: Chocolatey
---

# AWS Skill

Manage AWS resources via the CLI.

## Identity

```bash
aws sts get-caller-identity
```

## S3

List buckets:
```bash
aws s3 ls
```

List objects:
```bash
aws s3 ls s3://bucket-name/prefix/
```

Copy files:
```bash
aws s3 cp file.txt s3://bucket/path/
aws s3 cp s3://bucket/path/file.txt ./
```

Sync directory:
```bash
aws s3 sync ./local-dir s3://bucket/remote-dir/
```

## EC2

List instances:
```bash
aws ec2 describe-instances --query 'Reservations[].Instances[].{ID:InstanceId,State:State.Name,Type:InstanceType,IP:PublicIpAddress}' --output table
```

Start/stop:
```bash
aws ec2 start-instances --instance-ids i-xxx
aws ec2 stop-instances --instance-ids i-xxx
```

## Lambda

List functions:
```bash
aws lambda list-functions --query 'Functions[].FunctionName'
```

Invoke:
```bash
aws lambda invoke --function-name my-func --payload '{"key":"value"}' output.json
```

## CloudWatch Logs

Tail logs:
```bash
aws logs tail /aws/lambda/my-func --follow
```

## Guidelines

- Always specify `--region` or set `AWS_DEFAULT_REGION`
- Use `--output table` for human-readable output
- Use `--query` (JMESPath) to filter results
- Never hardcode credentials — use env vars or IAM roles
- Use `--dry-run` when available to preview changes
