FROM php:8.5-cli-bookworm

COPY --from=composer:2 /usr/bin/composer /usr/local/bin/composer
COPY --from=node:24-bookworm-slim /usr/local/bin/node /usr/local/bin/node
COPY --from=node:24-bookworm-slim /usr/local/lib/node_modules /usr/local/lib/node_modules

RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
    git unzip libzip-dev libicu-dev libonig-dev \
    && docker-php-ext-install zip intl mbstring pdo_mysql \
    && rm -rf /var/lib/apt/lists/*

ENV COMPOSER_ALLOW_SUPERUSER=1
ENV COMPOSER_HOME=/opt/composer
ENV PATH="/opt/composer/vendor/bin:${PATH}"

RUN composer global require laravel/installer \
    && chmod -R a+rX /opt/composer \
    && chmod 755 /opt/composer/vendor/bin/laravel

WORKDIR /workspace
