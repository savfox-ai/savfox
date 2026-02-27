---
name: database
description: Query and manage databases (PostgreSQL, MySQL, SQLite).
version: "1.0.0"
metadata:
  savfox:
    emoji: "🗄️"
    requires:
      bins: []
      env: []
    install: []
---

# Database Skill

Query and manage databases.

## PostgreSQL

Connect:
```bash
psql postgresql://user:password@host:5432/dbname
```

List databases:
```bash
psql -l -h host -U user
```

Run query:
```bash
psql -h host -U user -d dbname -c "SELECT * FROM users LIMIT 10;"
```

Run SQL file:
```bash
psql -h host -U user -d dbname -f script.sql
```

Export to CSV:
```bash
psql -h host -U user -d dbname -c "COPY (SELECT * FROM users) TO STDOUT WITH CSV HEADER" > users.csv
```

## MySQL

Connect:
```bash
mysql -h host -u user -p dbname
```

Run query:
```bash
mysql -h host -u user -p -e "SELECT * FROM users LIMIT 10;" dbname
```

## SQLite

Open database:
```bash
sqlite3 database.db
```

Run query:
```bash
sqlite3 database.db "SELECT * FROM users LIMIT 10;"
```

Export to CSV:
```bash
sqlite3 -header -csv database.db "SELECT * FROM users;" > users.csv
```

Schema:
```bash
sqlite3 database.db ".schema"
```

## Common Operations

Show tables (PostgreSQL):
```bash
psql -c "\dt" -h host -U user dbname
```

Show tables (MySQL):
```bash
mysql -e "SHOW TABLES;" -h host -u user -p dbname
```

## Guidelines

- Always use parameterized queries to prevent SQL injection
- Use `LIMIT` when exploring unknown tables
- Back up before running UPDATE or DELETE
- Use transactions for multi-statement operations
- Never store database passwords in plain text
