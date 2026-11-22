from setuptools import setup, find_packages

setup(
    name="appmgr",
    version="1.4.0",
    description="Universal .deb and .AppImage Application Manager (Pacman + APT hybrid)",
    author="Alex Zahn",
    packages=find_packages(),
    scripts=["bin/appmgr"],
    python_requires=">=3.8",
    classifiers=[
        "Environment :: Console",
        "Operating System :: POSIX :: Linux",
        "Programming Language :: Python :: 3",
        "Topic :: System :: Systems Administration",
    ],
)
