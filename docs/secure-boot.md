# Secure Boot

With Secure Boot on, the firmware only starts programs signed by a key in its
database. Slingshot isn't signed by Microsoft, so it needs one of these.

## With your own keys (sbctl)

If you manage your own keys with `sbctl`, `slingshot-setup install` signs
Slingshot and its drivers: it asks when Secure Boot is on, and `--sign` does
it without asking, even before you turn Secure Boot on. By hand (and your
kernels the same way):

```sh
# ⚠ /boot: replace with your ESP
sudo sbctl sign -s /boot/EFI/slingshot/slingshot.efi
```

## With your distribution's shim

Most distributions (Ubuntu, Fedora, openSUSE, Debian…) boot through
**shim**, a small loader signed by Microsoft that knows the distribution's
key. Slingshot can sit behind shim like GRUB: it then has shim check each
kernel, starting your distribution's signed kernels and refusing the others.

1. Put shim and Slingshot in Slingshot's folder (shim starts the
   `grubx64.efi` next to it). The commands use `/boot/efi`, where these
   distributions mount the EFI System Partition (`bootctl --print-esp-path`
   says), and `EFI/ubuntu`, your distribution's folder there (`EFI/fedora`…):

   ```sh
   # ⚠ /boot/efi: your ESP; ubuntu: your distribution's folder there
   sudo cp /boot/efi/EFI/ubuntu/shimx64.efi /boot/efi/EFI/slingshot/shimx64.efi
   sudo cp /boot/efi/EFI/slingshot/slingshot.efi /boot/efi/EFI/slingshot/grubx64.efi
   ```

2. Sign Slingshot with a key of your own, and enroll it in shim (a "MOK"):

   ```sh
   openssl req -new -x509 -newkey rsa:2048 -nodes -days 3650 \
       -subj "/CN=My Slingshot key/" -keyout MOK.key -out MOK.crt
   openssl x509 -in MOK.crt -outform DER -out MOK.der
   # ⚠ /boot/efi: your ESP
   sudo sbsign --key MOK.key --cert MOK.crt \
       --output /boot/efi/EFI/slingshot/grubx64.efi /boot/efi/EFI/slingshot/slingshot.efi
   sudo mokutil --import MOK.der      # asks for a one-time password
   ```

   On the next restart, shim's blue MokManager screen appears: choose "Enroll
   MOK" and type that password. Keep `MOK.key` safe: sign each new Slingshot
   with it.

3. Point Slingshot's boot entry at shim:

   ```sh
   sudo efibootmgr --create --disk /dev/nvme0n1 --part 1 --label Slingshot \
       --loader '\EFI\slingshot\shimx64.efi'
   ```

slingshot.toml stays next to Slingshot. Windows and anything signed by
Microsoft start as before. An unsigned kernel or program is refused with a
message.

Slingshot has the SBAT metadata shim requires (a `.sbat` section), so shim
can revoke a vulnerable version.

## Measured boot (TPM)

With a TPM, Slingshot measures what systemd-boot does: the kernel command
line (PCR 12) and `loader/loader.conf` when there is one (PCR 5). The
firmware measures the programs started (PCR 4), Linux its initrd (PCR 9). So
TPM-unlocked disks (`systemd-cryptenroll --tpm2-device=auto`) and
`systemd-pcrlock` see the same measurements whether Slingshot or
systemd-boot started the kernel.
