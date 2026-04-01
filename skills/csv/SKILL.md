---
name: csv
description: Process and analyze CSV files using csvkit, awk, and Python.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📊"
    requires:
      bins: []
      env: []
    install: []
---

# CSV Skill

Process and analyze CSV data.

## View CSV

Pretty print:
```bash
column -t -s',' data.csv | head -20
```

With csvkit:
```bash
csvlook data.csv
```

## Query CSV with SQL

```bash
csvsql --query "SELECT name, SUM(amount) FROM data GROUP BY name" data.csv
```

## Filter Rows

```bash
csvgrep -c "status" -m "active" data.csv
```

## Select Columns

```bash
csvcut -c 1,3,5 data.csv
csvcut -c "name,email" data.csv
```

## Sort

```bash
csvsort -c "amount" -r data.csv
```

## Statistics

```bash
csvstat data.csv
```

## Convert

JSON to CSV:
```bash
python3 -c "import json,csv,sys; data=json.load(sys.stdin); w=csv.DictWriter(sys.stdout,data[0].keys()); w.writeheader(); w.writerows(data)" < data.json
```

CSV to JSON:
```bash
python3 -c "import json,csv,sys; print(json.dumps(list(csv.DictReader(sys.stdin))))" < data.csv
```

## Python One-liners

Row count:
```bash
python3 -c "import csv; print(sum(1 for _ in csv.reader(open('data.csv'))))"
```

## Guidelines

- Use `csvkit` (`pip install csvkit`) for comprehensive CSV processing
- Always check encoding — use `file -i data.csv` to detect
- Use `head -1` to inspect column headers before processing
- For very large files, use `awk` instead of Python for speed
