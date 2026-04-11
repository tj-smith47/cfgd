Name:           cfgd
Version:        {{ Version }}
Release:        1%{?dist}
Summary:        Declarative, GitOps-style machine configuration management
License:        MIT
URL:            https://github.com/tj-smith47/cfgd
Source0:        %{name}-%{version}-source.tar.gz

%description
cfgd is a declarative, GitOps-style machine configuration state
management tool.  It ships a cross-platform CLI, a long-running
reconciliation daemon, a Kubernetes operator, and a CSI Node plugin
for injecting modules into Kubernetes pods.

%prep
%autosetup -n %{name}-%{version}

%build
cargo build --release --bin cfgd

%install
install -D -m 0755 target/release/cfgd %{buildroot}%{_bindir}/cfgd
install -D -m 0644 LICENSE %{buildroot}%{_datadir}/doc/%{name}/LICENSE
install -D -m 0644 README.md %{buildroot}%{_datadir}/doc/%{name}/README.md

%files
%{_bindir}/cfgd
%{_datadir}/doc/%{name}/LICENSE
%{_datadir}/doc/%{name}/README.md

%changelog
