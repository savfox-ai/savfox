---
name: email
description: Send and manage emails using command-line tools.
version: "1.0.0"
metadata:
  savfox:
    emoji: "📧"
    requires:
      bins: []
      env: []
    install: []
---

# Email Skill

Send and manage emails from the command line.

## Send with curl (SMTP)

```bash
curl --ssl-reqd \
  --url "smtps://smtp.gmail.com:465" \
  --user "user@gmail.com:app-password" \
  --mail-from "user@gmail.com" \
  --mail-rcpt "recipient@example.com" \
  -T - <<EOF
From: user@gmail.com
To: recipient@example.com
Subject: Hello from Savfox

This is the email body.
EOF
```

## Send with mailx

```bash
echo "Email body here" | mailx -s "Subject" recipient@example.com
```

## Send with Python

```bash
python3 -c "
import smtplib
from email.message import EmailMessage
msg = EmailMessage()
msg.set_content('Hello from Savfox')
msg['Subject'] = 'Test Email'
msg['From'] = 'sender@example.com'
msg['To'] = 'recipient@example.com'
with smtplib.SMTP_SSL('smtp.gmail.com', 465) as s:
    s.login('sender@example.com', 'app-password')
    s.send_message(msg)
print('Sent!')
"
```

## Read IMAP

```bash
python3 -c "
import imaplib, email
m = imaplib.IMAP4_SSL('imap.gmail.com')
m.login('user@gmail.com', 'app-password')
m.select('INBOX')
_, nums = m.search(None, 'UNSEEN')
for n in nums[0].split()[-5:]:
    _, data = m.fetch(n, '(RFC822)')
    msg = email.message_from_bytes(data[0][1])
    print(f'{msg[\"From\"]}: {msg[\"Subject\"]}')
m.logout()
"
```

## Guidelines

- Use App Passwords for Gmail (not your regular password)
- Always use SSL/TLS for SMTP and IMAP connections
- Be careful with email sending rate limits
- Never hardcode passwords — use environment variables
