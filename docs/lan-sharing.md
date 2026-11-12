# Local names and file sharing

The OSyx IPv4 resolver sends `.local` lookups to the multicast address
`224.0.0.251:5353` on connected interfaces. It uses an ephemeral source
port so peers can answer with legacy unicast mDNS replies. Successful
answers are cached for up to ten seconds. A `.local` lookup does not go to
a configured DNS server, including when mDNS times out.

This supports looking up other devices' IPv4 names. OSyx does not yet
advertise its own `.local` name or browse DNS-SD services. See
[RFC 6762, sections 5.1 and 6.7](https://www.rfc-editor.org/rfc/rfc6762.html#section-5.1).

In Files, select one regular file and choose **Send to phone…** from its
context menu. Scan the QR code with a phone on the same IPv4 subnet. The
window shows the URL if you need to type it instead. The listener uses
one connected IPv4 interface and a random URL token. It stops after one
completed download, five minutes, or when you close the window. A partial
download can be retried while the session is open.

The URL grants access to the selected file. HTTP sends the file and token
without encryption, so use this feature on a trusted local network. Wi-Fi
client isolation can prevent the phone from reaching OSyx.
