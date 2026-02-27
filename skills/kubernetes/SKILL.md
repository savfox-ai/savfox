---
name: kubernetes
description: Manage Kubernetes clusters and workloads with kubectl.
version: "1.0.0"
metadata:
  savfox:
    emoji: "⎈"
    requires:
      bins:
        - kubectl
      env: []
    install:
      - id: brew
        kind: brew
        formula: kubernetes-cli
        bins: [kubectl]
        label: Homebrew
      - id: choco
        kind: choco
        package: kubernetes-cli
        bins: [kubectl]
        label: Chocolatey
---

# Kubernetes Skill

Manage Kubernetes clusters and workloads.

## Cluster Info

```bash
kubectl cluster-info
kubectl get nodes
kubectl get namespaces
```

## Pods

List pods:
```bash
kubectl get pods -n <namespace>
kubectl get pods --all-namespaces
```

Describe a pod:
```bash
kubectl describe pod <pod-name> -n <namespace>
```

View pod logs:
```bash
kubectl logs <pod-name> -n <namespace> --tail=100 -f
```

Exec into a pod:
```bash
kubectl exec -it <pod-name> -n <namespace> -- bash
```

## Deployments

```bash
kubectl get deployments -n <namespace>
kubectl rollout status deployment/<name> -n <namespace>
kubectl rollout restart deployment/<name> -n <namespace>
kubectl scale deployment/<name> --replicas=3 -n <namespace>
```

## Services

```bash
kubectl get svc -n <namespace>
kubectl port-forward svc/<name> 8080:80 -n <namespace>
```

## Apply/Delete

```bash
kubectl apply -f manifest.yaml
kubectl delete -f manifest.yaml
```

## Guidelines

- Always specify `-n <namespace>` to avoid operating on wrong namespace
- Use `kubectl get events --sort-by='.lastTimestamp'` for debugging
- Use `kubectl top pods` to check resource usage (requires metrics-server)
