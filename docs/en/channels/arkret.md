# Arkret Agent channel

Savfox's Arkret Agent runtime handles authorized message subscription and replies.
It does not send encrypted presence heartbeats over Arkret Signal: the current v1
Signal envelope requires an ordinary account-device sender, and no Agent endpoint
carrier is registered. An Agent key or a synthetic device ID cannot replace that
device authorization.

Pairing therefore does not request `ak.self.signal.command.send`. A connected
Savfox listener indicates local runtime connectivity, not a remotely published
Agent online-presence claim. Agent presence can be enabled only after the protocol
defines a valid carrier and the complete sender and recipient checks are implemented.
