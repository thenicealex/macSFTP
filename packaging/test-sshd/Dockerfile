FROM ubuntu:24.04

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends openssh-server \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash macsftp \
    && echo 'macsftp:macsftp-test-password' | chpasswd \
    && mkdir -p /run/sshd /home/macsftp/fixture \
    && printf 'password-auth-ok\n' > /home/macsftp/fixture/readme.txt \
    && chown -R macsftp:macsftp /home/macsftp \
    && ssh-keygen -A

COPY sshd_config /etc/ssh/sshd_config_macsftp

EXPOSE 2222

CMD ["/usr/sbin/sshd", "-D", "-e", "-f", "/etc/ssh/sshd_config_macsftp"]
